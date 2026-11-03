//! S06 data-level tests: migration, pool de-duplication, freshness and the
//! custom-CLI lifecycle.
//!
//! Every test uses a synthetic temporary data root. No real profile, real
//! account, real transcript or real CLI is touched.

use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;
use uuid::Uuid;

use crate::multicli::custom::CustomCliInput;
use crate::multicli::domain::{AgentDefinition, Host, Session, SessionOrigin};
use crate::multicli::error::ErrorCode;
use crate::multicli::service::MultiCliService;
use crate::multicli::store::{MigrationFault, SCHEMA_VERSION};
use crate::multicli::usage::model::{
    AccountProfile, IdentityStatus, QuotaPool, UsageSample, UsageSnapshot, UsageStatus,
};
use crate::multicli::usage::repo;
use crate::multicli::usage::{provider_for, UsageCenter, UsageRefreshRequest};

const HOST_ID: &str = "local-windows";

struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "multicli-s06-{name}-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&path).expect("create temp root");
        Self { path }
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn open(root: &TempRoot) -> MultiCliService {
    MultiCliService::open(&root.path).expect("open synthetic store")
}

fn seed(service: &MultiCliService) {
    service.register_host(&Host::local(HOST_ID)).unwrap();
    service
        .register_agent(&AgentDefinition {
            id: "codex".into(),
            vendor: "OpenAI".into(),
            display_name: "Codex CLI".into(),
        })
        .unwrap();
}

fn account(fingerprint: &str) -> AccountProfile {
    AccountProfile {
        id: format!("account-codex-{fingerprint}"),
        host_id: HOST_ID.into(),
        agent_id: "codex".into(),
        product: "Codex CLI".into(),
        vendor: "OpenAI".into(),
        label: "Codex · a***@example.com · plus".into(),
        identity_status: IdentityStatus::Confirmed,
        identity_fingerprint: Some(fingerprint.to_string()),
        plan: Some("plus".into()),
        source: "codex app-server account/read".into(),
        observed_at: crate::multicli::usage::model::now_iso8601(),
    }
}

fn pool_for(account: &AccountProfile, installation: &str) -> QuotaPool {
    QuotaPool {
        id: format!("pool-{}", Uuid::new_v4().simple()),
        pool_key: QuotaPool::key_for(
            "codex",
            account.identity_status,
            account.identity_fingerprint.as_deref(),
            Some(installation),
        ),
        host_id: HOST_ID.into(),
        agent_id: "codex".into(),
        product: "Codex CLI".into(),
        account_profile_id: Some(account.id.clone()),
        identity_status: account.identity_status,
        label: account.label.clone(),
        installation_count: 0,
        created_at: crate::multicli::usage::model::now_iso8601(),
        updated_at: crate::multicli::usage::model::now_iso8601(),
    }
}

#[test]
fn v7_migration_keeps_existing_rows_and_creates_the_usage_tables() {
    let root = TempRoot::new("migrate");
    {
        let service = MultiCliService::open_for_test(
            &root.path,
            MigrationFault {
                stop_after_version: Some(6),
                ..Default::default()
            },
        )
        .expect("open v6 store");
        assert_eq!(service.schema_version(), 6);
        seed(&service);
        service
            .ensure_session(&Session {
                id: "session-1".into(),
                agent_id: "codex".into(),
                host_id: HOST_ID.into(),
                source_namespace: "history:codex:codex:unconfirmed-local".into(),
                native_id: Some("native-1".into()),
                origin: SessionOrigin::HistoryOnly,
                title: "kept".into(),
            })
            .unwrap();
        service
            .execute_raw(
                "INSERT INTO user_session_notes(session_id, note, updated_at_ms)
                 VALUES ('session-1', 'note that must survive', 1)",
            )
            .unwrap();
    }
    let service = MultiCliService::open(&root.path).expect("migrate to v7");
    assert_eq!(service.schema_version(), SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 7);
    let report = service.migration_report();
    assert_eq!(report.from_version, 6);
    assert_eq!(report.to_version, 7);
    let backup = report.backup.as_ref().expect("v6 -> v7 backup");
    assert!(backup.path.exists(), "consistent backup must exist");

    let conn = Connection::open(service.database_path()).unwrap();
    let note: String = conn
        .query_row(
            "SELECT note FROM user_session_notes WHERE session_id = 'session-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(note, "note that must survive");
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sessions, 1);
    for table in [
        "usage_accounts",
        "usage_pools",
        "usage_snapshots",
        "usage_samples",
        "usage_provider_state",
        "custom_clis",
    ] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "{table} must exist after v7");
    }
    let enabled: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(enabled, 1, "foreign keys stay on after the migration");
}

#[test]
fn a_failed_v7_migration_rolls_back_and_keeps_v6_data() {
    let root = TempRoot::new("migrate-fail");
    {
        let service = MultiCliService::open_for_test(
            &root.path,
            MigrationFault {
                stop_after_version: Some(6),
                ..Default::default()
            },
        )
        .unwrap();
        seed(&service);
    }
    let error = match MultiCliService::open_for_test(
        &root.path,
        MigrationFault {
            fail_at: Some((7, 0)),
            ..Default::default()
        },
    ) {
        Ok(_) => panic!("injected v7 failure did not fail"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::Database);
    assert_eq!(error.details["failedVersion"], 7);
    assert_eq!(error.details["databaseVersion"], 6);
    assert!(PathBuf::from(error.details["backupPath"].as_str().unwrap()).exists());

    let conn = Connection::open(root.path.join("multicli.db")).unwrap();
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(version, 6, "the transaction left the schema at v6");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='usage_pools'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "no half-created usage table");
    let agents: i64 = conn
        .query_row("SELECT COUNT(*) FROM agents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(agents, 1, "existing rows survive a failed migration");
    assert!(root.path.join("multicli.db").exists());
}

#[test]
fn two_installations_of_one_confirmed_account_share_exactly_one_pool() {
    let root = TempRoot::new("dedupe");
    let service = open(&root);
    seed(&service);
    let profile = account("fingerprint-abc");
    {
        let conn = service.store().connection().lock();
        let stored = repo::upsert_account(&conn, &profile).unwrap();
        let first = repo::upsert_pool(&conn, &pool_for(&stored, "install-1"), Some("install-1"))
            .unwrap();
        let second = repo::upsert_pool(&conn, &pool_for(&stored, "install-2"), Some("install-2"))
            .unwrap();
        assert_eq!(first.id, second.id, "one row for one confirmed account");
        assert_eq!(second.installation_count, 2);
        let pools = repo::all_pools(&conn, HOST_ID).unwrap();
        assert_eq!(pools.len(), 1);
        let ids = repo::installation_ids_for_pool(&conn, &pools[0].id).unwrap();
        assert_eq!(ids, vec!["install-1".to_string(), "install-2".to_string()]);

        // A different account is a different pool, never merged.
        let other = account("fingerprint-xyz");
        let stored_other = repo::upsert_account(&conn, &other).unwrap();
        repo::upsert_pool(&conn, &pool_for(&stored_other, "install-1"), Some("install-1")).unwrap();
        assert_eq!(repo::all_pools(&conn, HOST_ID).unwrap().len(), 2);
    }
}

#[test]
fn an_unconfirmed_pool_is_never_merged_with_the_confirmed_one() {
    let root = TempRoot::new("unconfirmed");
    let service = open(&root);
    seed(&service);
    let confirmed = account("fingerprint-abc");
    {
        let conn = service.store().connection().lock();
        let stored = repo::upsert_account(&conn, &confirmed).unwrap();
        repo::upsert_pool(&conn, &pool_for(&stored, "install-1"), Some("install-1")).unwrap();
        let mut unconfirmed = pool_for(&stored, "install-1");
        unconfirmed.identity_status = IdentityStatus::Unconfirmed;
        unconfirmed.account_profile_id = None;
        unconfirmed.pool_key = QuotaPool::key_for(
            "codex",
            IdentityStatus::Unconfirmed,
            None,
            Some("install-1"),
        );
        repo::upsert_pool(&conn, &unconfirmed, Some("install-1")).unwrap();
        let pools = repo::all_pools(&conn, HOST_ID).unwrap();
        assert_eq!(pools.len(), 2);
        assert!(pools.iter().any(|pool| pool.identity_status == IdentityStatus::Confirmed));
        assert!(pools
            .iter()
            .any(|pool| pool.identity_status == IdentityStatus::Unconfirmed));
    }
}

#[test]
fn a_stale_snapshot_is_reported_as_stale_and_never_as_zero() {
    let root = TempRoot::new("stale");
    let service = open(&root);
    seed(&service);
    let profile = account("fingerprint-abc");
    {
        let conn = service.store().connection().lock();
        let stored = repo::upsert_account(&conn, &profile).unwrap();
        let pool = repo::upsert_pool(&conn, &pool_for(&stored, "install-1"), Some("install-1"))
            .unwrap();
        drop(conn);
        let mut conn = service.store().connection().lock();
        let mut snapshot = UsageSnapshot::empty(&pool.id, "codex");
        snapshot.status = UsageStatus::Ok;
        snapshot.captured_at = crate::multicli::history::ids::iso_from_millis(
            crate::multicli::store::now_ms() - 60 * 60 * 1000,
        );
        snapshot.expires_at = Some(crate::multicli::history::ids::iso_from_millis(
            crate::multicli::store::now_ms() - 55 * 60 * 1000,
        ));
        snapshot.samples.push(UsageSample::quota(
            "Codex · primary",
            Some(42.0),
            Some(300),
            None,
            "test",
        ));
        repo::insert_snapshot(&mut conn, &snapshot).unwrap();
        let read = repo::latest_snapshot(&conn, &pool.id).unwrap().unwrap();
        assert_eq!(read.freshness, "stale");
        assert_eq!(read.status, UsageStatus::Ok);
        assert_eq!(read.samples.len(), 1);
        assert_eq!(read.samples[0].used_percent, Some(42.0));
    }
}

#[test]
fn a_failed_read_is_stored_as_a_real_error_without_numbers() {
    let root = TempRoot::new("failure");
    let service = open(&root);
    seed(&service);
    let profile = account("fingerprint-abc");
    {
        let conn = service.store().connection().lock();
        let stored = repo::upsert_account(&conn, &profile).unwrap();
        let pool = repo::upsert_pool(&conn, &pool_for(&stored, "install-1"), Some("install-1"))
            .unwrap();
        drop(conn);
        let mut conn = service.store().connection().lock();
        let mut snapshot = UsageSnapshot::empty(&pool.id, "codex");
        snapshot.status = UsageStatus::RateLimited;
        snapshot.error = Some(crate::multicli::error::AppError::new(
            crate::multicli::error::ErrorCode::Unsupported,
            "rate limited",
        ));
        snapshot.retry_after_ms = Some(120_000);
        snapshot.next_attempt_at = Some(crate::multicli::usage::model::iso_after_ms(120_000));
        snapshot.note = Some("no values this pass".into());
        repo::insert_snapshot(&mut conn, &snapshot).unwrap();
        let read = repo::latest_snapshot(&conn, &pool.id).unwrap().unwrap();
        assert_eq!(read.status, UsageStatus::RateLimited);
        assert!(read.samples.is_empty(), "a failure carries no numbers");
        assert!(read.error.is_some());
        assert_eq!(read.retry_after_ms, Some(120_000));
        assert!(
            repo::last_ok_snapshot(&conn, &pool.id).unwrap().is_none(),
            "a failed read is not a successful reading"
        );
    }
}

#[test]
fn provider_backoff_round_trips_and_success_clears_it() {
    let root = TempRoot::new("backoff");
    let service = open(&root);
    let conn = service.store().connection().lock();
    let error = crate::multicli::error::AppError::new(
        crate::multicli::error::ErrorCode::Timeout,
        "timed out",
    );
    let until = crate::multicli::usage::model::iso_after_ms(60_000);
    let state =
        repo::record_provider_attempt(&conn, "codex", UsageStatus::Error, false, Some(&until), Some(&error))
            .unwrap();
    assert_eq!(state.attempts, 1);
    assert_eq!(state.backoff_until.as_deref(), Some(until.as_str()));
    assert!(state.last_error.is_some());
    let again = repo::provider_state(&conn, "codex").unwrap();
    assert_eq!(again.attempts, 1);
    let cleared = repo::record_provider_attempt(
        &conn,
        "codex",
        UsageStatus::Ok,
        true,
        None,
        None,
    )
    .unwrap();
    assert_eq!(cleared.attempts, 0);
    assert!(cleared.backoff_until.is_none());
    assert!(cleared.last_success_at.is_some());
}

#[test]
fn a_custom_cli_round_trips_and_refuses_a_duplicate() {
    let root = TempRoot::new("custom");
    let service = open(&root);
    seed(&service);
    let exe = root.path.join("tool.exe");
    fs::write(&exe, b"MZ").unwrap();
    let input = CustomCliInput {
        label: "我的工具".into(),
        executable: exe.display().to_string(),
        args: vec!["--version".into(), "中文 参数".into()],
        cwd: Some(root.path.display().to_string()),
        product_hint: Some("custom".into()),
    };
    let created = service.create_custom_cli(HOST_ID, &input).unwrap();
    assert_eq!(created.history_access, "unsupported");
    assert_eq!(created.quota_access, "unsupported");
    assert_eq!(created.verification, "user-local-import");
    assert_eq!(created.args, input.args);
    let listed = service.list_custom_clis(HOST_ID).unwrap();
    assert_eq!(listed.len(), 1);
    let duplicate = service.create_custom_cli(HOST_ID, &input).unwrap_err();
    assert_eq!(duplicate.code, ErrorCode::Conflict);
    service.delete_custom_cli(HOST_ID, &created.id).unwrap();
    assert!(service.list_custom_clis(HOST_ID).unwrap().is_empty());
    assert!(service.delete_custom_cli(HOST_ID, &created.id).is_err());
}

#[test]
fn a_custom_cli_with_a_shell_host_is_refused_by_the_service() {
    let root = TempRoot::new("custom-shell");
    let service = open(&root);
    let cmd = PathBuf::from(
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32\\cmd.exe"),
    );
    if !cmd.is_file() {
        return;
    }
    let error = service
        .create_custom_cli(
            HOST_ID,
            &CustomCliInput {
                label: "cmd".into(),
                executable: cmd.display().to_string(),
                args: vec!["/c".into(), "whoami".into()],
                cwd: None,
                product_hint: None,
            },
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert!(service.list_custom_clis(HOST_ID).unwrap().is_empty());
}

#[test]
fn refreshing_without_an_installation_creates_no_empty_pool() {
    let root = TempRoot::new("refresh-none");
    let service = open(&root);
    seed(&service);
    let center = crate::multicli::usage::UsageCenter::new(
        HOST_ID,
        std::sync::Arc::new(service),
        root.path.clone(),
    );
    let result = center
        .refresh(&UsageRefreshRequest {
            agent_id: Some("codex".into()),
            force: true,
        })
        .unwrap();
    assert_eq!(result.outcomes.len(), 1);
    assert!(result.outcomes[0].skipped);
    assert_eq!(result.outcomes[0].status, UsageStatus::Unsupported);
    assert!(result.overview.unconfirmed_pools.is_empty());
    assert!(result.overview.accounts.is_empty());
}

#[test]
fn a_snapshot_without_samples_is_never_the_last_successful_reading() {
    let root = TempRoot::new("last-good");
    let service = open(&root);
    seed(&service);
    let profile = account("fingerprint-abc");
    let conn = service.store().connection().lock();
    let stored = repo::upsert_account(&conn, &profile).unwrap();
    let pool = repo::upsert_pool(&conn, &pool_for(&stored, "install-1"), Some("install-1")).unwrap();
    drop(conn);
    let mut conn = service.store().connection().lock();
    let mut empty = UsageSnapshot::empty(&pool.id, "codex");
    empty.status = UsageStatus::Ok;
    empty.note = Some("provider answered with no values".into());
    repo::insert_snapshot(&mut conn, &empty).unwrap();
    assert!(
        repo::last_ok_snapshot(&conn, &pool.id).unwrap().is_none(),
        "an ok row with no samples measured nothing"
    );
}

#[test]
fn a_failure_reuses_the_confirmed_pool_of_the_same_installation() {
    let root = TempRoot::new("reuse-pool");
    let service = open(&root);
    seed(&service);
    let profile = account("fingerprint-abc");
    let conn = service.store().connection().lock();
    let stored = repo::upsert_account(&conn, &profile).unwrap();
    let confirmed = repo::upsert_pool(&conn, &pool_for(&stored, "install-1"), Some("install-1"))
        .unwrap();
    let reused = repo::confirmed_pool_for_installation(&conn, HOST_ID, "codex", "install-1")
        .unwrap()
        .expect("confirmed pool");
    assert_eq!(reused.id, confirmed.id);
    // A different installation has no confirmed pool yet.
    assert!(
        repo::confirmed_pool_for_installation(&conn, HOST_ID, "codex", "install-9")
            .unwrap()
            .is_none()
    );
    // Confirming the account supersedes the old unconfirmed row for the same
    // installation, so the same allowance is not shown twice.
    let mut unconfirmed = pool_for(&stored, "install-1");
    unconfirmed.identity_status = IdentityStatus::Unconfirmed;
    unconfirmed.account_profile_id = None;
    unconfirmed.pool_key = QuotaPool::key_for("codex", IdentityStatus::Unconfirmed, None, Some("install-1"));
    repo::upsert_pool(&conn, &unconfirmed, Some("install-1")).unwrap();
    assert_eq!(repo::all_pools(&conn, HOST_ID).unwrap().len(), 2);
    let removed =
        repo::delete_unconfirmed_pool_for_installation(&conn, HOST_ID, "codex", "install-1").unwrap();
    assert_eq!(removed, 1);
    let pools = repo::all_pools(&conn, HOST_ID).unwrap();
    assert_eq!(pools.len(), 1);
    assert_eq!(pools[0].identity_status, IdentityStatus::Confirmed);
}

fn seeded_pool(service: &MultiCliService) -> QuotaPool {
    let profile = account("fingerprint-backoff");
    let conn = service.store().connection().lock();
    let stored = repo::upsert_account(&conn, &profile).unwrap();
    repo::upsert_pool(&conn, &pool_for(&stored, "install-1"), Some("install-1")).unwrap()
}

#[test]
fn a_quota_rate_limit_keeps_its_backoff_and_a_later_success_clears_it() {
    let root = TempRoot::new("persist-backoff");
    let service = std::sync::Arc::new(open(&root));
    seed(&service);
    let pool = seeded_pool(&service);
    let center = UsageCenter::new(HOST_ID, service.clone(), root.path.clone());
    let spec = provider_for("codex").unwrap();

    let mut limited = UsageSnapshot::empty(&pool.id, "codex");
    limited.status = UsageStatus::RateLimited;
    limited.retry_after_ms = Some(120_000);
    limited.error = Some(crate::multicli::error::AppError::new(
        crate::multicli::error::ErrorCode::Unsupported,
        "rate limited",
    ));
    let stored = center.persist_snapshot(spec, limited).unwrap();
    assert_eq!(stored.attempts, 1);
    assert!(stored.next_attempt_at.is_some());
    {
        let conn = service.store().connection().lock();
        let state = repo::provider_state(&conn, "codex").unwrap();
        assert_eq!(state.attempts, 1);
        assert!(state.backoff_until.is_some());
        assert!(state.last_error.is_some());
    }

    // An `unsupported` answer is neutral: it records the attempt but must not
    // clear the backoff the 429 earned.
    let mut neutral = UsageSnapshot::empty(&pool.id, "codex");
    neutral.status = UsageStatus::Unsupported;
    center.persist_snapshot(spec, neutral).unwrap();
    {
        let conn = service.store().connection().lock();
        let state = repo::provider_state(&conn, "codex").unwrap();
        assert_eq!(state.attempts, 1, "a neutral answer does not reset attempts");
        assert!(state.backoff_until.is_some(), "the backoff is preserved");
    }

    let mut ok = UsageSnapshot::empty(&pool.id, "codex");
    ok.status = UsageStatus::Ok;
    ok.samples.push(UsageSample::quota(
        "Codex · primary",
        Some(10.0),
        Some(300),
        None,
        "test",
    ));
    center.persist_snapshot(spec, ok).unwrap();
    {
        let conn = service.store().connection().lock();
        let state = repo::provider_state(&conn, "codex").unwrap();
        assert_eq!(state.attempts, 0);
        assert!(state.backoff_until.is_none());
        assert!(state.last_success_at.is_some());
    }
}

#[test]
fn a_backoff_survives_a_restart_because_it_is_stored() {
    let root = TempRoot::new("backoff-restart");
    {
        let service = std::sync::Arc::new(open(&root));
        seed(&service);
        let pool = seeded_pool(&service);
        let center = UsageCenter::new(HOST_ID, service.clone(), root.path.clone());
        let mut limited = UsageSnapshot::empty(&pool.id, "codex");
        limited.status = UsageStatus::RateLimited;
        limited.retry_after_ms = Some(120_000);
        limited.error = Some(crate::multicli::error::AppError::new(
            crate::multicli::error::ErrorCode::Unsupported,
            "rate limited",
        ));
        center
            .persist_snapshot(provider_for("codex").unwrap(), limited)
            .unwrap();
    }
    // A new service instance reads the same database: the backoff is a stored
    // fact, not in-memory state, so a restart does not hammer the provider.
    let reopened = MultiCliService::open(&root.path).unwrap();
    let conn = reopened.store().connection().lock();
    let state = repo::provider_state(&conn, "codex").unwrap();
    assert_eq!(state.attempts, 1);
    assert!(state.backoff_until.is_some());
    assert!(state.last_error.is_some());
}

#[test]
fn a_corrupt_custom_cli_argv_is_refused_by_the_schema() {
    let root = TempRoot::new("custom-corrupt");
    let service = open(&root);
    seed(&service);
    // `args_json` is an array by CHECK, so a corrupt value cannot be stored at
    // all; the Rust-side parse error is a second line of defence for a database
    // edited outside the application.
    let error = service
        .execute_raw(
            "INSERT INTO custom_clis(
                id, host_id, label, executable_display_path, executable_canonical_path,
                args_json, cwd, product_hint, created_at_ms, updated_at_ms
             ) VALUES ('custom-bad', 'local-windows', 'bad',
                'C:\\x.exe', 'C:\\x.exe',
                '{}', NULL, NULL, 1, 1)",
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Database);
    assert!(service.list_custom_clis(HOST_ID).unwrap().is_empty());
}

#[test]
fn an_unknown_stored_metric_is_refused_by_the_schema() {
    let root = TempRoot::new("bad-metric");
    let service = open(&root);
    seed(&service);
    let profile = account("fingerprint-abc");
    let conn = service.store().connection().lock();
    let stored = repo::upsert_account(&conn, &profile).unwrap();
    let pool = repo::upsert_pool(&conn, &pool_for(&stored, "install-1"), Some("install-1")).unwrap();
    drop(conn);
    let mut conn = service.store().connection().lock();
    let snapshot = UsageSnapshot::empty(&pool.id, "codex");
    repo::insert_snapshot(&mut conn, &snapshot).unwrap();
    // A metric outside the vocabulary cannot be stored, so it can never be
    // displayed as if it were a quota window.
    let error = conn
        .execute(
            "INSERT INTO usage_samples(
                snapshot_id, ordinal, metric, unit, window, label, estimated, source
             ) VALUES (?1, 0, 'quota-windwo', 'percent', 'short', 'x', 0, 'test')",
            [snapshot.id.as_str()],
        )
        .unwrap_err();
    let mapped = crate::multicli::AppError::from(error);
    assert_eq!(mapped.code, ErrorCode::Database);
    let read = repo::latest_snapshot(&conn, &pool.id).unwrap().unwrap();
    assert!(read.samples.is_empty());
}

#[test]
fn all_null_sample_rows_do_not_clear_backoff_or_record_success() {
    use super::{UsageCenter, provider_for};
    use std::sync::Arc;
    let root = TempRoot::new("all-null-samples");
    let service = Arc::new(open(&root));
    seed(&service);
    let pool = {
        let conn = service.store().connection().lock();
        let account = repo::upsert_account(&conn, &account("null-sample-account")).unwrap();
        repo::upsert_pool(&conn, &pool_for(&account, "installation"), Some("installation")).unwrap()
    };
    let center = UsageCenter::new(HOST_ID, service.clone(), root.path.clone());
    let mut limited = UsageSnapshot::empty(&pool.id, "codex");
    limited.status = UsageStatus::RateLimited;
    limited.retry_after_ms = Some(120_000);
    center.persist_snapshot(provider_for("codex").unwrap(), limited).unwrap();
    let before = {
        let conn = service.store().connection().lock();
        repo::provider_state(&conn, "codex").unwrap()
    };
    let mut empty = UsageSnapshot::empty(&pool.id, "codex");
    empty.status = UsageStatus::Partial;
    empty.samples.push(UsageSample::token_measurement("unknown tokens", super::model::UsageWindow::Daily,
        super::model::TokenSemantics::InputExcludesCached, None, None, None, None, None, "synthetic"));
    center.persist_snapshot(provider_for("codex").unwrap(), empty).unwrap();
    let conn = service.store().connection().lock();
    let after = repo::provider_state(&conn, "codex").unwrap();
    assert_eq!(after.backoff_until, before.backoff_until);
    assert_eq!(after.attempts, before.attempts);
    assert!(after.last_success_at.is_none());
}
