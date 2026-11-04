//! S02 engine-level tests with a controlled host backend.
//!
//! These tests exercise the real detector, the real signature registry, the
//! real SQLite store and the real view/redaction pipeline. Only the Windows
//! process boundary is replaced, so the tests can construct PID reuse, access
//! denial and total enumeration failure deterministically.

use super::detect::ScannerEnvironment;
use super::engine::DiscoveryEngine;
use super::snapshot::ProcessState;
use super::view::{InstallationQuery, ProcessQuery};
use super::winproc::{BackendError, HostBackend, ProcessDetails, RawProcess};
use crate::multicli::{Host, MultiCliService};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const HOST: &str = "local-windows";

fn temp_root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "multicli-engine-{name}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&path).unwrap();
    // The controlled backend should return the same physical image spelling as
    // the detector. Windows CI may give temp_dir() a short name or junction
    // alias; retaining it here made the fake image miss the canonical index.
    PathBuf::from(crate::multicli::path::canonical_text(
        &std::fs::canonicalize(&path).unwrap(),
    ))
}

#[derive(Default)]
struct FakeBackend {
    rows: Mutex<Vec<RawProcess>>,
    details: Mutex<HashMap<u32, Result<ProcessDetails, BackendError>>>,
    fail_enumeration: AtomicBool,
}

impl FakeBackend {
    fn set_rows(&self, rows: Vec<RawProcess>, details: Vec<ProcessDetails>) {
        *self.rows.lock().unwrap() = rows;
        let mut map = HashMap::new();
        for detail in details {
            map.insert(detail.pid, Ok(detail));
        }
        *self.details.lock().unwrap() = map;
    }

    fn deny(&self, pid: u32) {
        self.details
            .lock()
            .unwrap()
            .insert(pid, Err(BackendError::access_denied(pid)));
    }

    fn fail(&self, fail: bool) {
        self.fail_enumeration.store(fail, Ordering::SeqCst);
    }
}

impl HostBackend for FakeBackend {
    fn enumerate(&self) -> Result<Vec<RawProcess>, BackendError> {
        if self.fail_enumeration.load(Ordering::SeqCst) {
            return Err(BackendError::failed(
                None,
                "enumeration refused by test double",
            ));
        }
        Ok(self.rows.lock().unwrap().clone())
    }

    fn inspect(&self, pid: u32) -> Result<ProcessDetails, BackendError> {
        self.details
            .lock()
            .unwrap()
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| Err(BackendError::gone(pid)))
    }

    fn resolve_executable(&self, pid: u32) -> Result<Option<PathBuf>, BackendError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|row| row.pid == pid)
            .and_then(|row| row.executable.clone()))
    }
}

struct Fixture {
    root: PathBuf,
    service: Arc<MultiCliService>,
    backend: Arc<FakeBackend>,
    engine: Arc<DiscoveryEngine>,
    exe: PathBuf,
}

fn fixture(name: &str) -> Fixture {
    let root = temp_root(name);
    let bin = root.join("local/Programs/OpenAI/Codex/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("codex.exe");
    std::fs::write(&exe, b"MZ not a real executable").unwrap();
    let service = Arc::new(MultiCliService::open(root.join("data")).unwrap());
    service.register_host(&Host::local(HOST)).unwrap();
    let environment = ScannerEnvironment {
        path_dirs: vec![bin.clone()],
        path_truncated: false,
        user_profile: Some(root.join("profile")),
        local_app_data: Some(root.join("local")),
        app_data: Some(root.join("roaming")),
        program_files: Some(root.join("programs")),
    };
    let backend = Arc::new(FakeBackend::default());
    let engine = Arc::new(DiscoveryEngine::new(
        HOST,
        &root.join("data"),
        service.clone(),
        backend.clone(),
        environment,
    ));
    Fixture {
        root,
        service,
        backend,
        engine,
        exe,
    }
}

fn raw(pid: u32, parent: Option<u32>, exe: &Path, argv: &[&str]) -> RawProcess {
    RawProcess {
        pid,
        parent_pid: parent,
        name: exe.file_name().unwrap().to_string_lossy().to_string(),
        executable: Some(exe.to_path_buf()),
        argv: argv.iter().map(|item| item.to_string()).collect(),
        start_time_hint: 0,
    }
}

fn details(
    pid: u32,
    created: &str,
    parent: Option<u32>,
    exe: &Path,
    argv: &[&str],
) -> ProcessDetails {
    ProcessDetails {
        pid,
        creation_time: created.to_string(),
        executable: Some(exe.to_path_buf()),
        argv: argv.iter().map(|item| item.to_string()).collect(),
        parent_pid: parent,
    }
}

#[test]
fn restart_reconciles_absent_persisted_process_only_after_successful_enumeration() {
    let fixture = fixture("restart-persisted-process");
    let argv = [fixture.exe.to_str().unwrap()];
    fixture.backend.set_rows(
        vec![raw(4242, None, &fixture.exe, &argv)],
        vec![details(4242, "2026-11-02T01:00:00.1234567Z", None, &fixture.exe, &argv)],
    );
    fixture.engine.run_scan();
    let alive = || {
        let conn = fixture.service.store().connection().lock();
        crate::multicli::repo::alive_discovery_processes(&conn, "codex").unwrap().len()
    };
    assert_eq!(alive(), 1);
    let restarted = DiscoveryEngine::new(
        HOST, &fixture.root.join("data"), fixture.service.clone(),
        fixture.backend.clone(), ScannerEnvironment {
            path_dirs: vec![fixture.exe.parent().unwrap().to_path_buf()],
            path_truncated: false,
            user_profile: None, local_app_data: None, app_data: None, program_files: None,
        },
    );
    fixture.backend.set_rows(Vec::new(), Vec::new());
    fixture.backend.fail(true);
    restarted.run_scan();
    assert_eq!(alive(), 1, "enumeration failure cannot prove exit");
    fixture.backend.fail(false);
    fixture.backend.set_rows(vec![raw(4242, None, &fixture.exe, &argv)], Vec::new());
    fixture.backend.deny(4242);
    restarted.run_scan();
    assert_eq!(alive(), 1, "a present but unreadable PID cannot prove exit");
    fixture.backend.set_rows(Vec::new(), Vec::new());
    restarted.run_scan();
    assert_eq!(alive(), 0, "successful absence must release the stale resume blocker");
}

#[test]
fn scan_finds_one_installation_and_one_instance_without_duplicates() {
    let fixture = fixture("basic");
    let argv = [
        fixture.exe.to_string_lossy().to_string(),
        "--version".to_string(),
    ];
    fixture.backend.set_rows(
        vec![raw(
            4242,
            Some(4),
            &fixture.exe,
            &argv.iter().map(|v| v.as_str()).collect::<Vec<_>>(),
        )],
        vec![details(
            4242,
            "2026-11-02T01:00:00.1234567Z",
            Some(4),
            &fixture.exe,
            &argv.iter().map(|v| v.as_str()).collect::<Vec<_>>(),
        )],
    );

    fixture.engine.run_scan();
    fixture.engine.run_scan();

    let installations = fixture
        .engine
        .list_installations(&InstallationQuery {
            agent_id: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    assert_eq!(installations.total, 1, "repeat scans must not duplicate");
    assert_eq!(installations.items[0].agent_id, "codex");
    assert_eq!(installations.items[0].status, "installed");
    assert_eq!(installations.items[0].config_profile_status, "unconfirmed");

    let processes = fixture
        .engine
        .list_processes(&ProcessQuery {
            agent_id: None,
            workspace_id: None,
            state: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    assert_eq!(processes.total, 1);
    assert_eq!(processes.items[0].state, "alive");
    assert_eq!(processes.items[0].freshness, "fresh");
    assert_eq!(
        processes.items[0].identity.as_ref().unwrap().creation_time,
        "2026-11-02T01:00:00.1234567Z"
    );
    assert_eq!(processes.scan.status, "ok");
    assert!(processes
        .scan
        .note
        .contains("shorter than the scan interval"));
    assert_eq!(processes.items[0].session_id, None);

    // The persisted rows carry the same identity and no raw command line.
    let conn = fixture.service.lock_for_test();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM discovery_processes", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    let identities: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_installations WHERE agent_id = 'codex'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(identities, 1);
}

#[test]
fn total_enumeration_failure_keeps_rows_stale_and_recovers() {
    let fixture = fixture("total-failure");
    let argv = [fixture.exe.to_string_lossy().to_string()];
    fixture.backend.set_rows(
        vec![raw(500, None, &fixture.exe, &[argv[0].as_str()])],
        vec![details(
            500,
            "2026-11-02T01:00:00.0000001Z",
            None,
            &fixture.exe,
            &[argv[0].as_str()],
        )],
    );
    fixture.engine.run_scan();
    let healthy = fixture.engine.scan_view();
    assert_eq!(healthy.status, "ok");

    fixture.backend.fail(true);
    fixture.engine.run_scan();
    let failed = fixture.engine.scan_view();
    assert_eq!(failed.status, "partial");
    assert!(failed.error.is_some());
    assert_eq!(failed.last_success_at, healthy.last_success_at);

    let page = fixture
        .engine
        .list_processes(&ProcessQuery {
            agent_id: None,
            workspace_id: None,
            state: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    assert_eq!(page.items.len(), 1, "the previous row must stay visible");
    assert_eq!(
        page.items[0].state, "alive",
        "failure must not fake an exit"
    );
    assert_eq!(page.items[0].freshness, "stale");

    fixture.backend.fail(false);
    fixture.engine.run_scan();
    let recovered = fixture
        .engine
        .list_processes(&ProcessQuery {
            agent_id: None,
            workspace_id: None,
            state: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    assert_eq!(recovered.items[0].freshness, "fresh");
    assert_eq!(recovered.scan.status, "ok");
}

#[test]
fn access_denied_is_unknown_and_pid_reuse_gets_a_new_identity() {
    let fixture = fixture("denied-reuse");
    let argv = [fixture.exe.to_string_lossy().to_string()];
    fixture.backend.set_rows(
        vec![raw(700, None, &fixture.exe, &[argv[0].as_str()])],
        vec![details(
            700,
            "2026-11-02T01:00:00.0000001Z",
            None,
            &fixture.exe,
            &[argv[0].as_str()],
        )],
    );
    fixture.engine.run_scan();

    // The PID exists but the creation time can no longer be read.
    fixture.backend.deny(700);
    fixture.engine.run_scan();
    let page = fixture
        .engine
        .list_processes(&ProcessQuery {
            agent_id: None,
            workspace_id: None,
            state: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    assert_eq!(page.items[0].state, "unknown");
    assert_eq!(page.items[0].state_reason.as_deref(), Some("access-denied"));

    // Same PID, new creation time: a new identity, the old one exits.
    fixture.backend.set_rows(
        vec![raw(700, None, &fixture.exe, &[argv[0].as_str()])],
        vec![details(
            700,
            "2026-11-02T02:00:00.0000001Z",
            None,
            &fixture.exe,
            &[argv[0].as_str()],
        )],
    );
    fixture.engine.run_scan();
    let page = fixture
        .engine
        .list_processes(&ProcessQuery {
            agent_id: None,
            workspace_id: None,
            state: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    let alive = page
        .items
        .iter()
        .find(|item| item.state == "alive")
        .expect("the reused PID is a new live instance");
    assert_eq!(
        alive.identity.as_ref().unwrap().creation_time,
        "2026-11-02T02:00:00.0000001Z"
    );
    let exited = page
        .items
        .iter()
        .find(|item| item.state == "exited")
        .expect("the previous identity is closed");
    assert_eq!(exited.state_reason.as_deref(), Some("pid-reused"));
    assert_eq!(
        exited.identity.as_ref().unwrap().creation_time,
        "2026-11-02T01:00:00.0000001Z"
    );
}

#[test]
fn sensitive_arguments_never_reach_ipc_or_the_database() {
    let fixture = fixture("redaction");
    let secret_prompt = "SENTINEL-PROMPT-refactor-private-repo";
    let secret_token = "sk-live-SENTINEL-TOKEN-123456";
    let url = "https://example.test/run?token=SENTINEL-URL&account=me";
    let argv = [
        fixture.exe.to_string_lossy().to_string(),
        "--model".to_string(),
        secret_token.to_string(),
        "exec".to_string(),
        secret_prompt.to_string(),
        url.to_string(),
    ];
    let argv_refs = argv.iter().map(|value| value.as_str()).collect::<Vec<_>>();
    fixture.backend.set_rows(
        vec![raw(900, None, &fixture.exe, &argv_refs)],
        vec![details(
            900,
            "2026-11-02T01:00:00.0000001Z",
            None,
            &fixture.exe,
            &argv_refs,
        )],
    );
    fixture.engine.run_scan();

    let page = fixture
        .engine
        .list_processes(&ProcessQuery {
            agent_id: None,
            workspace_id: None,
            state: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    let wire = serde_json::to_string(&page).unwrap();
    let redacted = page.items[0].redacted_command.clone().unwrap();
    assert!(redacted.contains("codex.exe"));
    assert!(redacted.contains("--model <value>"));
    for forbidden in [secret_prompt, secret_token, "SENTINEL-URL", "example.test"] {
        assert!(!wire.contains(forbidden), "IPC leaked {forbidden}: {wire}");
        assert!(!redacted.contains(forbidden));
    }

    let stored: Vec<String> = {
        // Scope the connection guard: the service uses the same mutex for
        // every operation, so holding it across the export would deadlock.
        let conn = fixture.service.lock_for_test();
        let mut statement = conn
            .prepare("SELECT COALESCE(redacted_command, '') FROM discovery_processes")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|value| value.unwrap())
            .collect::<Vec<_>>();
        rows
    };
    let joined = stored.join("\n");
    for forbidden in [secret_prompt, secret_token, "SENTINEL-URL", "example.test"] {
        assert!(
            !joined.contains(forbidden),
            "database leaked {forbidden}: {joined}"
        );
    }

    // A diagnostics export is built from whitelisted counts only.
    let export = fixture.service.export_diagnostics("test").unwrap();
    let document = std::fs::read_to_string(&export.path).unwrap();
    for forbidden in [secret_prompt, secret_token, "SENTINEL-URL"] {
        assert!(!document.contains(forbidden));
    }
}

#[test]
fn filters_and_limits_are_validated_and_pages_are_stable() {
    let fixture = fixture("filters");
    let query = ProcessQuery {
        agent_id: None,
        workspace_id: None,
        state: None,
        limit: 0,
        cursor: None,
    };
    assert_eq!(
        fixture.engine.list_processes(&query).unwrap_err().code,
        crate::multicli::ErrorCode::InvalidArgument
    );
    let query = ProcessQuery {
        agent_id: None,
        workspace_id: None,
        state: Some(ProcessState::Alive),
        limit: 201,
        cursor: None,
    };
    assert!(fixture.engine.list_processes(&query).is_err());
    assert!(fixture
        .engine
        .list_installations(&InstallationQuery {
            agent_id: None,
            limit: 20,
            cursor: Some("garbage".to_string()),
        })
        .is_err());
}

#[test]
fn settings_are_bounded_and_persisted_in_the_project_profile() {
    let fixture = fixture("settings");
    assert_eq!(fixture.engine.interval_ms(), 3000);
    let error = fixture
        .engine
        .update_settings(&crate::multicli::discovery::view::DiscoverySettingsUpdate {
            interval_ms: 200,
        })
        .unwrap_err();
    assert_eq!(error.code, crate::multicli::ErrorCode::InvalidArgument);
    let applied = fixture
        .engine
        .update_settings(&crate::multicli::discovery::view::DiscoverySettingsUpdate {
            interval_ms: 5000,
        })
        .unwrap();
    assert_eq!(applied.interval_ms, 5000);
    let settings_file = fixture.root.join("data/discovery-settings.json");
    assert!(settings_file.is_file());
    let text = std::fs::read_to_string(&settings_file).unwrap();
    assert!(text.contains("\"intervalMs\": 5000"));
    assert!(!text.contains(&fixture.root.display().to_string()));
}

#[test]
fn refresh_shares_one_scan_and_reports_the_generation() {
    let fixture = fixture("refresh");
    fixture.backend.set_rows(vec![], vec![]);
    let first = fixture.engine.refresh();
    assert!(first.generation >= 1);
    assert_eq!(first.status, "ok");
    let second = fixture.engine.refresh();
    assert!(second.generation > first.generation);
}

#[test]
fn inspected_replacement_is_not_attributed_to_the_enumerated_cli() {
    let fixture = fixture("inspect-race");
    let arguments = [fixture.exe.to_str().unwrap()];
    fixture.backend.set_rows(
        vec![raw(4242, None, &fixture.exe, &arguments)],
        vec![details(
            4242,
            "2026-11-02T01:00:00.1234567Z",
            None,
            &fixture.root.join("ordinary.exe"),
            &["ordinary.exe"],
        )],
    );
    fixture.engine.run_scan();
    let page = fixture
        .engine
        .list_processes(&ProcessQuery {
            agent_id: None,
            workspace_id: None,
            state: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    assert_eq!(
        page.total, 0,
        "replacement process must not inherit the old image's product"
    );
}

#[test]
fn missing_recognition_with_present_identity_becomes_unknown() {
    let fixture = fixture("degraded");
    let arguments = [fixture.exe.to_str().unwrap()];
    let row = raw(4242, None, &fixture.exe, &arguments);
    let detail = details(
        4242,
        "2026-11-02T01:00:00.1234567Z",
        None,
        &fixture.exe,
        &arguments,
    );
    fixture
        .backend
        .set_rows(vec![row.clone()], vec![detail.clone()]);
    fixture.engine.run_scan();
    let mut unreadable = row;
    unreadable.executable = None;
    unreadable.argv.clear();
    fixture.backend.set_rows(vec![unreadable], vec![detail]);
    fixture.engine.run_scan();
    let page = fixture
        .engine
        .list_processes(&ProcessQuery {
            agent_id: None,
            workspace_id: None,
            state: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].state, "unknown");
    assert_eq!(page.items[0].freshness, "stale");
}

#[test]
fn argv_program_name_cannot_replace_the_os_image() {
    let registry = super::signature::SignatureRegistry::standard();
    let mut index = super::classify::InstallationIndex::default();
    index
        .trusted
        .path_entries
        .push(super::signature::normalize_key(r"C:\Synthetic\bin"));
    let row = raw(
        12345,
        None,
        Path::new(r"C:\Synthetic\ordinary.exe"),
        &[r"C:\Synthetic\bin\codex.exe", "--help"],
    );
    assert!(super::classify::recognize(&row, &index, &registry).is_none());
}

#[test]
fn a_native_filename_on_path_is_unverified_and_never_probed() {
    let root = temp_root("unrelated-path");
    std::fs::write(root.join("codex.exe"), b"MZ ordinary synthetic program").unwrap();
    let environment = ScannerEnvironment {
        path_dirs: vec![root],
        ..Default::default()
    };
    let scan = super::detect::scan_installations(
        &super::signature::SignatureRegistry::standard(),
        &environment,
        HOST,
    );
    let candidate = scan
        .installations
        .iter()
        .find(|i| i.agent_id == "codex")
        .unwrap();
    assert_eq!(candidate.status, super::detect::InstallStatus::Unverified);
    assert!(!candidate.is_runnable());
}
