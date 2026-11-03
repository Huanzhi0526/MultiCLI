//! SQL access for accounts, quota pools, snapshots and custom CLIs.
//!
//! Every read reconstructs the domain object from its own row(s); nothing is
//! inferred. A missing snapshot is `None` (the caller renders `unknown`), never
//! a synthetic `ok` with zero samples.

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::store::now_ms;

use super::model::{
    AccountProfile, IdentityStatus, QuotaPool, TokenSemantics, UsageMetric, UsageSample,
    UsageSnapshot, UsageSourceKind, UsageStatus, UsageUnit, UsageWindow,
};

/// Snapshots kept per pool. Older rows are deleted so a 5-minute refresh cannot
/// grow the database without bound.
pub const SNAPSHOTS_PER_POOL: i64 = 40;

fn text_enum_parse<T, F>(value: &str, field: &str, parse: F) -> Result<T, AppError>
where
    F: Fn(&str) -> Option<T>,
{
    parse(value).ok_or_else(|| {
        AppError::new(
            ErrorCode::Database,
            format!("unexpected {field} value stored in the usage tables: {value}"),
        )
    })
}

pub(crate) fn identity_status_from_db(value: &str) -> Option<IdentityStatus> {
    match value {
        "confirmed" => Some(IdentityStatus::Confirmed),
        "unconfirmed" => Some(IdentityStatus::Unconfirmed),
        "unknown" => Some(IdentityStatus::Unknown),
        _ => None,
    }
}

fn usage_status_from_db(value: &str) -> Option<UsageStatus> {
    UsageStatus::parse(value)
}

fn source_kind_from_db(value: &str) -> Option<UsageSourceKind> {
    match value {
        "official-machine" => Some(UsageSourceKind::OfficialMachine),
        "local-transcript" => Some(UsageSourceKind::LocalTranscript),
        "manual-entry" => Some(UsageSourceKind::ManualEntry),
        "unavailable" => Some(UsageSourceKind::Unavailable),
        _ => None,
    }
}

fn metric_from_db(value: &str) -> Option<UsageMetric> {
    match value {
        "quota-window" => Some(UsageMetric::QuotaWindow),
        "token-usage" => Some(UsageMetric::TokenUsage),
        "credit-balance" => Some(UsageMetric::CreditBalance),
        "cost-estimate" => Some(UsageMetric::CostEstimate),
        "billing" => Some(UsageMetric::Billing),
        _ => None,
    }
}

fn unit_from_db(value: &str) -> Option<UsageUnit> {
    match value {
        "percent" => Some(UsageUnit::Percent),
        "tokens" => Some(UsageUnit::Tokens),
        "credits" => Some(UsageUnit::Credits),
        "usd-micros" => Some(UsageUnit::UsdMicros),
        "count" => Some(UsageUnit::Count),
        _ => None,
    }
}

fn window_from_db(value: &str) -> Option<UsageWindow> {
    match value {
        "short" => Some(UsageWindow::Short),
        "daily" => Some(UsageWindow::Daily),
        "weekly" => Some(UsageWindow::Weekly),
        "monthly" => Some(UsageWindow::Monthly),
        "lifetime" => Some(UsageWindow::Lifetime),
        "session" => Some(UsageWindow::Session),
        "unspecified" => Some(UsageWindow::Unspecified),
        _ => None,
    }
}

fn semantics_from_db(value: Option<String>) -> Result<Option<TokenSemantics>, AppError> {
    match value.as_deref() {
        None => Ok(None),
        Some("input-includes-cached") => Ok(Some(TokenSemantics::InputIncludesCached)),
        Some("input-excludes-cached") => Ok(Some(TokenSemantics::InputExcludesCached)),
        Some(other) => Err(AppError::new(
            ErrorCode::Database,
            format!("unexpected token semantics stored in the usage tables: {other}"),
        )),
    }
}

fn u64_of(value: Option<i64>) -> Option<u64> {
    value.and_then(|item| u64::try_from(item).ok())
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

pub(crate) fn upsert_account(
    conn: &Connection,
    account: &AccountProfile,
) -> Result<AccountProfile, AppError> {
    account.validate()?;
    let stamp = now_ms();
    // A confirmed identity is keyed by its fingerprint, so the same account
    // reaching us through a second installation or session updates one row
    // instead of creating a second one.
    let existing: Option<String> = match &account.identity_fingerprint {
        Some(fingerprint) => conn
            .query_row(
                "SELECT id FROM usage_accounts
                 WHERE host_id = ?1 AND agent_id = ?2 AND identity_fingerprint = ?3",
                params![account.host_id, account.agent_id, fingerprint],
                |row| row.get(0),
            )
            .optional()?,
        None => conn
            .query_row(
                "SELECT id FROM usage_accounts WHERE id = ?1",
                params![account.id],
                |row| row.get(0),
            )
            .optional()?,
    };
    let id = existing.unwrap_or_else(|| account.id.clone());
    conn.execute(
        "INSERT INTO usage_accounts(
            id, host_id, agent_id, product, vendor, label, identity_status,
            identity_fingerprint, plan, source, observed_at, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
         ON CONFLICT(id) DO UPDATE SET
            label = excluded.label,
            identity_status = excluded.identity_status,
            identity_fingerprint = COALESCE(excluded.identity_fingerprint, usage_accounts.identity_fingerprint),
            plan = COALESCE(excluded.plan, usage_accounts.plan),
            source = excluded.source,
            observed_at = excluded.observed_at,
            updated_at_ms = excluded.updated_at_ms",
        params![
            id,
            account.host_id,
            account.agent_id,
            account.product,
            account.vendor,
            account.label,
            account.identity_status.as_str(),
            account.identity_fingerprint,
            account.plan,
            account.source,
            account.observed_at,
            stamp,
        ],
    )
    .map_err(|error| {
        AppError::from(error).with_details(serde_json::json!({ "stage": "upsertAccount" }))
    })?;
    let mut stored = account.clone();
    stored.id = id;
    Ok(stored)
}

fn map_account(row: &Row<'_>) -> rusqlite::Result<AccountProfile> {
    let identity_status: String = row.get("identity_status")?;
    Ok(AccountProfile {
        id: row.get("id")?,
        host_id: row.get("host_id")?,
        agent_id: row.get("agent_id")?,
        product: row.get("product")?,
        vendor: row.get("vendor")?,
        label: row.get("label")?,
        // A stored value outside the enum is surfaced as `unknown` rather than
        // guessed; the row's own label still explains what it was.
        identity_status: identity_status_from_db(&identity_status)
            .unwrap_or(IdentityStatus::Unknown),
        identity_fingerprint: row.get("identity_fingerprint")?,
        plan: row.get("plan")?,
        source: row.get("source")?,
        observed_at: row.get("observed_at")?,
    })
}

pub(crate) fn account_by_id(
    conn: &Connection,
    id: &str,
) -> Result<Option<AccountProfile>, AppError> {
    Ok(conn
        .query_row(
            "SELECT id, host_id, agent_id, product, vendor, label, identity_status,
                    identity_fingerprint, plan, source, observed_at
             FROM usage_accounts WHERE id = ?1",
            params![id],
            map_account,
        )
        .optional()?)
}

pub(crate) fn all_accounts(
    conn: &Connection,
    host_id: &str,
) -> Result<Vec<AccountProfile>, AppError> {
    let mut statement = conn.prepare(
        "SELECT id, host_id, agent_id, product, vendor, label, identity_status,
                identity_fingerprint, plan, source, observed_at
         FROM usage_accounts WHERE host_id = ?1
         ORDER BY agent_id, updated_at_ms DESC, id",
    )?;
    let rows = statement.query_map(params![host_id], map_account)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// Pools
// ---------------------------------------------------------------------------

fn read_installation_ids(raw: &str) -> Result<Vec<String>, AppError> {
    serde_json::from_str::<Vec<String>>(raw).map_err(|error| {
        AppError::new(
            ErrorCode::Database,
            format!("usage_pools.installation_ids_json is not a string array: {error}"),
        )
    })
}

pub(crate) fn upsert_pool(
    conn: &Connection,
    pool: &QuotaPool,
    installation_id: Option<&str>,
) -> Result<QuotaPool, AppError> {
    pool.validate()?;
    let stamp = now_ms();
    let existing: Option<(String, String)> = conn
        .query_row(
            "SELECT id, installation_ids_json FROM usage_pools WHERE pool_key = ?1",
            params![pool.pool_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (id, mut installations) = match existing {
        Some((id, raw)) => (id, read_installation_ids(&raw)?),
        None => (pool.id.clone(), Vec::new()),
    };
    if let Some(installation) = installation_id {
        if !installations.iter().any(|item| item == installation) {
            installations.push(installation.to_string());
            installations.sort();
        }
    }
    let installations_json = serde_json::to_string(&installations)?;
    conn.execute(
        "INSERT INTO usage_pools(
            id, pool_key, host_id, agent_id, product, account_profile_id,
            identity_status, label, installation_ids_json, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
         ON CONFLICT(id) DO UPDATE SET
            account_profile_id = COALESCE(excluded.account_profile_id, usage_pools.account_profile_id),
            identity_status = excluded.identity_status,
            label = excluded.label,
            installation_ids_json = excluded.installation_ids_json,
            updated_at_ms = excluded.updated_at_ms",
        params![
            id,
            pool.pool_key,
            pool.host_id,
            pool.agent_id,
            pool.product,
            pool.account_profile_id,
            pool.identity_status.as_str(),
            pool.label,
            installations_json,
            stamp,
        ],
    )
    .map_err(|error| {
        AppError::from(error).with_details(serde_json::json!({ "stage": "upsertPool" }))
    })?;
    let mut stored = pool.clone();
    stored.id = id;
    stored.installation_count = installations.len() as u32;
    Ok(stored)
}

fn map_pool(row: &Row<'_>) -> rusqlite::Result<QuotaPool> {
    let identity_status: String = row.get("identity_status")?;
    let raw: String = row.get("installation_ids_json")?;
    let installations = read_installation_ids(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    Ok(QuotaPool {
        id: row.get("id")?,
        pool_key: row.get("pool_key")?,
        host_id: row.get("host_id")?,
        agent_id: row.get("agent_id")?,
        product: row.get("product")?,
        account_profile_id: row.get("account_profile_id")?,
        identity_status: identity_status_from_db(&identity_status)
            .unwrap_or(IdentityStatus::Unknown),
        label: row.get("label")?,
        installation_count: installations.len() as u32,
        created_at: crate::multicli::history::ids::iso_from_millis(row.get("created_at_ms")?),
        updated_at: crate::multicli::history::ids::iso_from_millis(row.get("updated_at_ms")?),
    })
}

pub(crate) fn all_pools(conn: &Connection, host_id: &str) -> Result<Vec<QuotaPool>, AppError> {
    let mut statement = conn.prepare(
        "SELECT id, pool_key, host_id, agent_id, product, account_profile_id,
                identity_status, label, installation_ids_json, created_at_ms, updated_at_ms
         FROM usage_pools WHERE host_id = ?1
         ORDER BY agent_id, updated_at_ms DESC, id",
    )?;
    let rows = statement.query_map(params![host_id], map_pool)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub(crate) fn installation_ids_for_pool(
    conn: &Connection,
    pool_id: &str,
) -> Result<Vec<String>, AppError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT installation_ids_json FROM usage_pools WHERE id = ?1",
            params![pool_id],
            |row| row.get(0),
        )
        .optional()?;
    match raw {
        Some(value) => read_installation_ids(&value),
        None => Ok(Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

fn map_sample(row: &Row<'_>) -> rusqlite::Result<UsageSample> {
    let metric: String = row.get("metric")?;
    let unit: String = row.get("unit")?;
    let window: String = row.get("window")?;
    let semantics = semantics_from_db(row.get("token_semantics")?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    // An unrecognised spelling must not be guessed: guessing `quota-window`
    // could present a bill as an allowance.
    let conversion = |name: &str, value: &str| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(crate::multicli::error::AppError::new(
                crate::multicli::error::ErrorCode::Database,
                format!("unexpected {name} value stored in usage_samples: {value}"),
            )),
        )
    };
    let metric = metric_from_db(&metric).ok_or_else(|| conversion("metric", &metric))?;
    let unit = unit_from_db(&unit).ok_or_else(|| conversion("unit", &unit))?;
    let window = window_from_db(&window).ok_or_else(|| conversion("window", &window))?;
    Ok(UsageSample {
        metric,
        unit,
        window,
        label: row.get("label")?,
        used_percent: row.get("used_percent")?,
        remaining_percent: row.get("remaining_percent")?,
        amount: row.get("amount")?,
        total_tokens: u64_of(row.get("total_tokens")?),
        input_tokens: u64_of(row.get("input_tokens")?),
        cached_input_tokens: u64_of(row.get("cached_input_tokens")?),
        cache_creation_tokens: u64_of(row.get("cache_creation_tokens")?),
        output_tokens: u64_of(row.get("output_tokens")?),
        token_semantics: semantics,
        resets_at: row.get("resets_at")?,
        window_minutes: u64_of(row.get("window_minutes")?).map(|value| value as u32),
        estimated: row.get::<_, i64>("estimated")? != 0,
        source: row.get("source")?,
        note: row.get("note")?,
    })
}

pub(crate) fn insert_snapshot(
    conn: &mut Connection,
    snapshot: &UsageSnapshot,
) -> Result<(), AppError> {
    snapshot.validate()?;
    let stamp = now_ms();
    let error_json = match &snapshot.error {
        Some(error) => Some(serde_json::to_string(error)?),
        None => None,
    };
    let unavailable = serde_json::to_string(&snapshot.unavailable_fields)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO usage_snapshots(
            id, pool_id, agent_id, status, source_kind, source, captured_at, expires_at,
            error_json, retry_after_ms, next_attempt_at, attempts, note,
            unavailable_fields_json, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            snapshot.id,
            snapshot.pool_id,
            snapshot.agent_id,
            snapshot.status.as_str(),
            snapshot.source_kind.as_str(),
            snapshot.source,
            snapshot.captured_at,
            snapshot.expires_at,
            error_json,
            snapshot.retry_after_ms,
            snapshot.next_attempt_at,
            snapshot.attempts as i64,
            snapshot.note,
            unavailable,
            stamp,
        ],
    )?;
    for (ordinal, sample) in snapshot.samples.iter().enumerate() {
        tx.execute(
            "INSERT INTO usage_samples(
                snapshot_id, ordinal, metric, unit, window, label, used_percent,
                remaining_percent, amount, total_tokens, input_tokens, cached_input_tokens,
                cache_creation_tokens, output_tokens, token_semantics, resets_at,
                window_minutes, estimated, source, note
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                snapshot.id,
                ordinal as i64,
                sample.metric.as_str(),
                sample.unit.as_str(),
                sample.window.as_str(),
                sample.label,
                sample.used_percent,
                sample.remaining_percent,
                sample.amount,
                sample.total_tokens.map(|value| value as i64),
                sample.input_tokens.map(|value| value as i64),
                sample.cached_input_tokens.map(|value| value as i64),
                sample.cache_creation_tokens.map(|value| value as i64),
                sample.output_tokens.map(|value| value as i64),
                sample.token_semantics.map(|value| value.as_str()),
                sample.resets_at,
                sample.window_minutes.map(|value| value as i64),
                if sample.estimated { 1_i64 } else { 0 },
                sample.source,
                sample.note,
            ],
        )?;
    }
    // Bound history: keep the newest N snapshots of this pool.
    tx.execute(
        "DELETE FROM usage_snapshots
         WHERE pool_id = ?1 AND id NOT IN (
            SELECT id FROM usage_snapshots WHERE pool_id = ?1
            ORDER BY created_at_ms DESC, id DESC LIMIT ?2
         )",
        params![snapshot.pool_id, SNAPSHOTS_PER_POOL],
    )?;
    tx.commit()?;
    Ok(())
}

fn map_snapshot(row: &Row<'_>, samples: Vec<UsageSample>, now: i64) -> rusqlite::Result<UsageSnapshot> {
    let status_raw: String = row.get("status")?;
    let source_kind_raw: String = row.get("source_kind")?;
    let error_json: Option<String> = row.get("error_json")?;
    let captured_at: String = row.get("captured_at")?;
    let unavailable_raw: String = row.get("unavailable_fields_json")?;
    let status = usage_status_from_db(&status_raw).unwrap_or(UsageStatus::Unknown);
    let ttl = {
        let expires: Option<String> = row.get("expires_at")?;
        expires
            .as_deref()
            .and_then(crate::multicli::history::ids::parse_iso_millis)
            .map(|deadline| deadline - crate::multicli::history::ids::parse_iso_millis(&captured_at).unwrap_or(deadline))
            .unwrap_or(super::model::DEFAULT_TTL_MS)
    };
    let freshness = super::model::freshness_at(&captured_at, ttl, now);
    Ok(UsageSnapshot {
        id: row.get("id")?,
        pool_id: row.get("pool_id")?,
        agent_id: row.get("agent_id")?,
        status,
        freshness: freshness.to_string(),
        source_kind: source_kind_from_db(&source_kind_raw).unwrap_or(UsageSourceKind::Unavailable),
        source: row.get("source")?,
        captured_at,
        expires_at: row.get("expires_at")?,
        error: error_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<AppError>(raw).ok()),
        retry_after_ms: row.get("retry_after_ms")?,
        next_attempt_at: row.get("next_attempt_at")?,
        attempts: row.get::<_, i64>("attempts")? as u32,
        samples,
        note: row.get("note")?,
        unavailable_fields: serde_json::from_str(&unavailable_raw).unwrap_or_default(),
    })
}

/// Newest snapshot of one pool, with its samples in stored order.
pub(crate) fn latest_snapshot(
    conn: &Connection,
    pool_id: &str,
) -> Result<Option<UsageSnapshot>, AppError> {
    snapshot_with_filter(conn, pool_id, "")
}

/// Newest snapshot that actually carried a measurement. It is shown as
/// "last successful reading" beside a failed refresh, so a stale number can
/// never be mistaken for the current state.
pub(crate) fn last_ok_snapshot(
    conn: &Connection,
    pool_id: &str,
) -> Result<Option<UsageSnapshot>, AppError> {
    // A snapshot without a single sample measured nothing, so it cannot be the
    // "last successful reading" either.
    snapshot_with_filter(
        conn,
        pool_id,
        "AND status IN ('ok', 'partial') AND EXISTS (
             SELECT 1 FROM usage_samples WHERE snapshot_id = usage_snapshots.id
         )",
    )
}

fn snapshot_with_filter(
    conn: &Connection,
    pool_id: &str,
    filter: &str,
) -> Result<Option<UsageSnapshot>, AppError> {
    let now = now_ms();
    let query = format!(
        "SELECT id FROM usage_snapshots WHERE pool_id = ?1 {filter}
         ORDER BY created_at_ms DESC, id DESC LIMIT 1"
    );
    let snapshot_id: Option<String> = conn
        .query_row(&query, params![pool_id], |row| row.get(0))
        .optional()?;
    let Some(snapshot_id) = snapshot_id else {
        return Ok(None);
    };
    let snapshot = conn
        .query_row(
            "SELECT id, pool_id, agent_id, status, source_kind, source, captured_at,
                    expires_at, error_json, retry_after_ms, next_attempt_at, attempts,
                    note, unavailable_fields_json
             FROM usage_snapshots WHERE id = ?1",
            params![snapshot_id],
            |row| {
                let mut samples_statement = conn.prepare(
                    "SELECT metric, unit, window, label, used_percent, remaining_percent,
                            amount, total_tokens, input_tokens, cached_input_tokens,
                            cache_creation_tokens, output_tokens, token_semantics,
                            resets_at, window_minutes, estimated, source, note
                     FROM usage_samples WHERE snapshot_id = ?1 ORDER BY ordinal",
                )?;
                let samples = samples_statement
                    .query_map(params![snapshot_id], map_sample)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                map_snapshot(row, samples, now)
            },
        )
        .optional()?;
    Ok(snapshot)
}

// ---------------------------------------------------------------------------
// Provider state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ProviderState {
    pub agent_id: String,
    pub last_attempt_at: Option<String>,
    pub last_success_at: Option<String>,
    pub attempts: u32,
    pub backoff_until: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<AppError>,
}

pub(crate) fn provider_state(
    conn: &Connection,
    agent_id: &str,
) -> Result<ProviderState, AppError> {
    let state = conn
        .query_row(
            "SELECT agent_id, last_attempt_at, last_success_at, attempts, backoff_until,
                    last_status, last_error_json
             FROM usage_provider_state WHERE agent_id = ?1",
            params![agent_id],
            |row| {
                let error_json: Option<String> = row.get("last_error_json")?;
                Ok(ProviderState {
                    agent_id: row.get("agent_id")?,
                    last_attempt_at: row.get("last_attempt_at")?,
                    last_success_at: row.get("last_success_at")?,
                    attempts: row.get::<_, i64>("attempts")? as u32,
                    backoff_until: row.get("backoff_until")?,
                    last_status: row.get("last_status")?,
                    last_error: error_json
                        .as_deref()
                        .and_then(|raw| serde_json::from_str::<AppError>(raw).ok()),
                })
            },
        )
        .optional()?;
    Ok(state.unwrap_or_else(|| ProviderState {
        agent_id: agent_id.to_string(),
        ..ProviderState::default()
    }))
}

/// Records one attempt. `success` resets the backoff counter; a retryable
/// failure keeps it growing.
pub(crate) fn record_provider_attempt(
    conn: &Connection,
    agent_id: &str,
    status: UsageStatus,
    success: bool,
    backoff_until: Option<&str>,
    error: Option<&AppError>,
) -> Result<ProviderState, AppError> {
    let mut state = provider_state(conn, agent_id)?;
    let now_iso = super::model::now_iso8601();
    state.last_attempt_at = Some(now_iso.clone());
    if success {
        state.last_success_at = Some(now_iso);
        state.attempts = 0;
        state.backoff_until = None;
        state.last_error = None;
    } else {
        state.attempts = state.attempts.saturating_add(1);
        state.backoff_until = backoff_until.map(str::to_string);
        state.last_error = error.cloned();
    }
    state.last_status = Some(status.as_str().to_string());
    save_provider_state(conn, &state)?;
    Ok(state)
}

/// Records an attempt that neither succeeded nor produced a retryable failure
/// (`unauthenticated`, `unsupported`, or an honest partial with no numbers).
///
/// `last_attempt_at`/`last_status` move so the user sees when it happened, but
/// `attempts`, `backoff_until` and `last_error` are left untouched: a neutral
/// answer can neither clear a real backoff nor make the next one longer.
pub(crate) fn record_neutral_attempt(
    conn: &Connection,
    agent_id: &str,
    status: UsageStatus,
) -> Result<ProviderState, AppError> {
    let mut state = provider_state(conn, agent_id)?;
    state.last_attempt_at = Some(super::model::now_iso8601());
    state.last_status = Some(status.as_str().to_string());
    save_provider_state(conn, &state)?;
    Ok(state)
}

fn save_provider_state(conn: &Connection, state: &ProviderState) -> Result<(), AppError> {
    let error_json = match &state.last_error {
        Some(error) => Some(serde_json::to_string(error)?),
        None => None,
    };
    conn.execute(
        "INSERT INTO usage_provider_state(
            agent_id, last_attempt_at, last_success_at, attempts, backoff_until,
            last_status, last_error_json, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(agent_id) DO UPDATE SET
            last_attempt_at = excluded.last_attempt_at,
            last_success_at = excluded.last_success_at,
            attempts = excluded.attempts,
            backoff_until = excluded.backoff_until,
            last_status = excluded.last_status,
            last_error_json = excluded.last_error_json,
            updated_at_ms = excluded.updated_at_ms",
        params![
            state.agent_id,
            state.last_attempt_at,
            state.last_success_at,
            state.attempts as i64,
            state.backoff_until,
            state.last_status,
            error_json,
            now_ms(),
        ],
    )?;
    Ok(())
}

pub(crate) fn all_provider_states(conn: &Connection) -> Result<Vec<ProviderState>, AppError> {
    let mut statement = conn.prepare(
        "SELECT agent_id FROM usage_provider_state ORDER BY agent_id",
    )?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    for id in ids {
        items.push(provider_state(conn, &id)?);
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// Configured read-only roots (S03 sources)
// ---------------------------------------------------------------------------

/// Enabled history roots of one product. Usage reads only these roots: it does
/// not invent a default home directory, matching S03's "only what the user
/// enabled" rule.
pub(crate) fn enabled_roots(
    conn: &Connection,
    agent_id: &str,
) -> Result<Vec<(String, String)>, AppError> {
    let mut statement = conn.prepare(
        "SELECT id, root_canonical_path FROM history_sources
         WHERE agent_id = ?1 AND enabled = 1 ORDER BY root_canonical_path",
    )?;
    let rows = statement.query_map(params![agent_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// Custom CLIs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CustomCliRow {
    pub id: String,
    pub host_id: String,
    pub label: String,
    pub executable_display_path: String,
    pub executable_canonical_path: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub product_hint: Option<String>,
    pub created_at: String,
}

fn map_custom_cli(row: &Row<'_>) -> rusqlite::Result<CustomCliRow> {
    let raw: String = row.get("args_json")?;
    let args: Vec<String> = serde_json::from_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(crate::multicli::error::AppError::new(
                crate::multicli::error::ErrorCode::Database,
                format!("custom_clis.args_json is not a string array: {error}"),
            )),
        )
    })?;
    Ok(CustomCliRow {
        id: row.get("id")?,
        host_id: row.get("host_id")?,
        label: row.get("label")?,
        executable_display_path: row.get("executable_display_path")?,
        executable_canonical_path: row.get("executable_canonical_path")?,
        args,
        cwd: row.get("cwd")?,
        product_hint: row.get("product_hint")?,
        created_at: crate::multicli::history::ids::iso_from_millis(row.get("created_at_ms")?),
    })
}

pub(crate) fn insert_custom_cli(
    conn: &Connection,
    row: &CustomCliRow,
) -> Result<CustomCliRow, AppError> {
    let args_json = serde_json::to_string(&row.args)?;
    let stamp = now_ms();
    conn.execute(
        "INSERT INTO custom_clis(
            id, host_id, label, executable_display_path, executable_canonical_path,
            args_json, cwd, product_hint, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        params![
            row.id,
            row.host_id,
            row.label,
            row.executable_display_path,
            row.executable_canonical_path,
            args_json,
            row.cwd,
            row.product_hint,
            stamp,
        ],
    )
    .map_err(|error| {
        AppError::from(error).with_details(serde_json::json!({ "stage": "insertCustomCli" }))
    })?;
    Ok(row.clone())
}

/// The confirmed pool of one installation, if any. A failed read must keep
/// reporting on the account's own pool instead of creating a second,
/// unconfirmed one for the same installation.
pub(crate) fn confirmed_pool_for_installation(
    conn: &Connection,
    host_id: &str,
    agent_id: &str,
    installation_id: &str,
) -> Result<Option<QuotaPool>, AppError> {
    Ok(conn
        .query_row(
            "SELECT id, pool_key, host_id, agent_id, product, account_profile_id,
                    identity_status, label, installation_ids_json, created_at_ms, updated_at_ms
             FROM usage_pools
             WHERE host_id = ?1 AND agent_id = ?2 AND identity_status = 'confirmed'
               AND EXISTS (
                   SELECT 1 FROM json_each(usage_pools.installation_ids_json) WHERE value = ?3
               )
             ORDER BY updated_at_ms DESC LIMIT 1",
            params![host_id, agent_id, installation_id],
            map_pool,
        )
        .optional()?)
}

/// Removes the superseded unconfirmed pool of one installation after the same
/// installation has been attributed to a confirmed account. Its snapshots go
/// with it (the FK cascades); the account pool keeps its own history.
pub(crate) fn delete_unconfirmed_pool_for_installation(
    conn: &Connection,
    host_id: &str,
    agent_id: &str,
    installation_id: &str,
) -> Result<usize, AppError> {
    let affected = conn.execute(
        "DELETE FROM usage_pools
         WHERE host_id = ?1 AND agent_id = ?2 AND identity_status <> 'confirmed'
           AND EXISTS (
               SELECT 1 FROM json_each(usage_pools.installation_ids_json) WHERE value = ?3
           )",
        params![host_id, agent_id, installation_id],
    )?;
    Ok(affected)
}

pub(crate) fn all_custom_clis(
    conn: &Connection,
    host_id: &str,
) -> Result<Vec<CustomCliRow>, AppError> {
    let mut statement = conn.prepare(
        "SELECT id, host_id, label, executable_display_path, executable_canonical_path,
                args_json, cwd, product_hint, created_at_ms
         FROM custom_clis WHERE host_id = ?1 ORDER BY created_at_ms DESC, id",
    )?;
    let rows = statement.query_map(params![host_id], map_custom_cli)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub(crate) fn custom_cli_by_id(
    conn: &Connection,
    id: &str,
) -> Result<Option<CustomCliRow>, AppError> {
    Ok(conn
        .query_row(
            "SELECT id, host_id, label, executable_display_path, executable_canonical_path,
                    args_json, cwd, product_hint, created_at_ms
             FROM custom_clis WHERE id = ?1",
            params![id],
            map_custom_cli,
        )
        .optional()?)
}

pub(crate) fn delete_custom_cli(conn: &Connection, id: &str) -> Result<bool, AppError> {
    let affected = conn.execute("DELETE FROM custom_clis WHERE id = ?1", params![id])?;
    Ok(affected > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_enum_values_are_parsed_not_guessed() {
        assert_eq!(
            identity_status_from_db("confirmed"),
            Some(IdentityStatus::Confirmed)
        );
        assert_eq!(identity_status_from_db("merged"), None);
        assert_eq!(usage_status_from_db("ok"), Some(UsageStatus::Ok));
        assert_eq!(usage_status_from_db("fine"), None);
        assert!(text_enum_parse::<IdentityStatus, _>("confirmed", "identity", identity_status_from_db).is_ok());
        assert!(text_enum_parse::<IdentityStatus, _>("what", "identity", identity_status_from_db).is_err());
    }
}
