//! MultiCLI startup boundary and product IPC registration.
//!
//! Upstream startup is retained for reference but never invoked: its
//! migrations, CLI skill installation and background hosts do not belong to
//! this application. This file is also the only place that registers product
//! commands; the domain module stays transport-independent.
use crate::multicli::{
    AgentListItem, AppError, Capability, CapabilityQuery, DiagnosticsExport, ErrorCode, Host,
    MultiCliService, Page, PageRequest, SessionDetail, SessionQuery, SessionSummary,
    WorkspaceListItem,
};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri::State;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselineInfo {
    product: &'static str,
    stage: &'static str,
    profile: String,
    data_root: String,
    upstream_commit: &'static str,
    disabled: [&'static str; 7],
}

fn selected_profile(value: Option<&str>, debug: bool) -> Result<&str, String> {
    let profile = value.unwrap_or(if debug { "dev" } else { "prod" });
    match profile {
        "dev" | "test" | "prod" => Ok(profile),
        _ => Err("MULTICLI_PROFILE must be dev, test or prod".into()),
    }
}

pub(crate) fn profile() -> Result<String, String> {
    let value = std::env::var("MULTICLI_PROFILE").ok();
    selected_profile(value.as_deref(), cfg!(debug_assertions)).map(str::to_owned)
}

fn data_root_for(base: &Path, profile: &str) -> Result<PathBuf, String> {
    if !base.is_absolute() {
        return Err("Local application data path must be absolute".into());
    }
    Ok(base
        .join("MultiCLI")
        .join(selected_profile(Some(profile), false)?))
}

pub(crate) fn data_root() -> Result<PathBuf, String> {
    let base = dirs::data_local_dir().ok_or("Local application data directory unavailable")?;
    data_root_for(&base, &profile()?)
}

/// Holds the lazily opened service. A failed open is remembered as a real
/// `AppError` and retried on the next command, so the UI can offer a retry
/// action instead of showing an empty database or crashing the process.
pub struct MultiCliRuntime {
    root: PathBuf,
    profile: String,
    state: Mutex<RuntimeState>,
}

#[derive(Default)]
struct RuntimeState {
    service: Option<Arc<MultiCliService>>,
    error: Option<AppError>,
}

impl MultiCliRuntime {
    pub fn new(root: PathBuf, profile: String) -> Self {
        Self {
            root,
            profile,
            state: Mutex::new(RuntimeState::default()),
        }
    }

    pub fn from_environment() -> Self {
        let raw_profile = std::env::var("MULTICLI_PROFILE").ok();
        let profile_value = raw_profile.clone().unwrap_or_else(|| {
            if cfg!(debug_assertions) {
                "dev".to_string()
            } else {
                "prod".to_string()
            }
        });
        match profile() {
            Ok(profile) => match data_root() {
                Ok(root) => Self::new(root, profile),
                Err(message) => {
                    let runtime = Self::new(
                        std::env::temp_dir().join("MultiCLI").join("unavailable"),
                        profile,
                    );
                    runtime.state.lock().error = Some(
                        AppError::new(ErrorCode::Io, message)
                            .with_details(serde_json::json!({ "stage": "resolveDataRoot" }))
                            .with_retryable(true),
                    );
                    runtime
                }
            },
            Err(message) => {
                // Keep the raw value for display instead of silently claiming
                // another profile. No database is opened while it is invalid.
                let runtime = Self::new(
                    std::env::temp_dir().join("MultiCLI").join("unavailable"),
                    profile_value,
                );
                runtime.state.lock().error = Some(
                    AppError::invalid_argument("profile", message)
                        .with_details(serde_json::json!({ "stage": "resolveDataRoot" })),
                );
                runtime
            }
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Opens the isolated database on first use. Each failed attempt is
    /// retryable without restarting the application.
    pub fn service(&self) -> Result<Arc<MultiCliService>, AppError> {
        let mut state = self.state.lock();
        if let Some(service) = &state.service {
            return Ok(service.clone());
        }
        let mut root = self.root.clone();
        if let Some(error) = &state.error {
            if error.details.get("stage").and_then(|value| value.as_str())
                == Some("resolveDataRoot")
            {
                // Re-resolve the profile root on every retry. This covers a
                // transiently unavailable local app-data directory without
                // requiring an application restart.
                if let Err(message) = profile() {
                    return Err(AppError::invalid_argument("profile", message)
                        .with_details(serde_json::json!({ "stage": "resolveDataRoot" })));
                }
                match data_root() {
                    Ok(resolved) => root = resolved,
                    Err(message) => {
                        let error = AppError::new(ErrorCode::Io, message)
                            .with_details(serde_json::json!({ "stage": "resolveDataRoot" }))
                            .with_retryable(true);
                        state.error = Some(error.clone());
                        return Err(error);
                    }
                }
            }
        }
        match MultiCliService::open(&root) {
            Ok(service) => {
                // The only host this stage can truthfully initialize is the
                // local Windows host. No agent/session/installation rows are
                // fabricated here.
                if let Err(error) = service.register_host(&Host::local("local-windows")) {
                    state.error = Some(error.clone());
                    return Err(error);
                }
                let service = Arc::new(service);
                state.service = Some(service.clone());
                state.error = None;
                Ok(service)
            }
            Err(error) => {
                let retryable = matches!(
                    error.code,
                    ErrorCode::Io | ErrorCode::Database | ErrorCode::Timeout
                );
                let error = error.with_retryable(retryable);
                state.error = Some(error.clone());
                Err(error)
            }
        }
    }
}

#[tauri::command]
fn multicli_baseline_info(runtime: State<'_, MultiCliRuntime>) -> Result<BaselineInfo, String> {
    Ok(BaselineInfo {
        product: "MultiCLI",
        stage: "S01 — navigation and diagnostics",
        profile: runtime.profile().to_string(),
        data_root: runtime.root().display().to_string(),
        upstream_commit: "6c46345a727e7c1b57e0dacecb4fe063d88ee8dc",
        disabled: [
            "remote",
            "relay",
            "telemetry",
            "computer-use",
            "plugins",
            "auto-update",
            "CLI-config-writes",
        ],
    })
}

fn list_workspaces_impl(
    service: &MultiCliService,
    query: &PageRequest,
) -> Result<Page<WorkspaceListItem>, AppError> {
    service.list_workspaces(query)
}

fn required_argument<T: serde::de::DeserializeOwned>(
    value: Option<serde_json::Value>,
    field: &str,
) -> Result<T, AppError> {
    let value = value.ok_or_else(|| AppError::invalid_argument(field, "is required"))?;
    serde_json::from_value(value)
        .map_err(|error| AppError::invalid_argument(field, error.to_string()))
}

#[tauri::command]
fn multicli_list_workspaces(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<Page<WorkspaceListItem>, AppError> {
    let query = required_argument(query, "query")?;
    let service = runtime.service()?;
    list_workspaces_impl(&service, &query)
}

fn list_agents_impl(
    service: &MultiCliService,
    query: &PageRequest,
) -> Result<Page<AgentListItem>, AppError> {
    service.list_agents(query)
}

#[tauri::command]
fn multicli_list_agents(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<Page<AgentListItem>, AppError> {
    let query = required_argument(query, "query")?;
    let service = runtime.service()?;
    list_agents_impl(&service, &query)
}

fn list_sessions_impl(
    service: &MultiCliService,
    query: &SessionQuery,
) -> Result<Page<SessionSummary>, AppError> {
    service.list_sessions(query)
}

#[tauri::command]
fn multicli_list_sessions(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<Page<SessionSummary>, AppError> {
    let query = required_argument(query, "query")?;
    let service = runtime.service()?;
    list_sessions_impl(&service, &query)
}

fn get_session_impl(
    service: &MultiCliService,
    session_id: &str,
) -> Result<SessionDetail, AppError> {
    service.get_session(session_id)
}

#[tauri::command]
fn multicli_get_session(
    session_id: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<SessionDetail, AppError> {
    let session_id: String = required_argument(session_id, "sessionId")?;
    let service = runtime.service()?;
    get_session_impl(&service, &session_id)
}

fn list_capabilities_impl(
    service: &MultiCliService,
    query: &CapabilityQuery,
) -> Result<Page<Capability>, AppError> {
    service.list_capabilities_page(query)
}

#[tauri::command]
fn multicli_list_capabilities(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<Page<Capability>, AppError> {
    let query = required_argument(query, "query")?;
    let service = runtime.service()?;
    list_capabilities_impl(&service, &query)
}

fn export_diagnostics_impl(
    service: &MultiCliService,
    profile: &str,
) -> Result<DiagnosticsExport, AppError> {
    service.export_diagnostics(profile)
}

#[tauri::command]
fn multicli_export_diagnostics(
    runtime: State<'_, MultiCliRuntime>,
) -> Result<DiagnosticsExport, AppError> {
    let service = runtime.service()?;
    export_diagnostics_impl(&service, runtime.profile())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PtySmoke {
    input_echo: bool,
    unicode_cwd: bool,
    resize: bool,
    exit_success: bool,
}

fn shell_smoke(root: &Path) -> Result<PtySmoke, String> {
    let cwd = root.join("smoke 中文 space");
    std::fs::create_dir_all(&cwd).map_err(|e| format!("create smoke directory: {e}"))?;
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("create PTY: {e}"))?;
    let system_root = std::env::var_os("SystemRoot").ok_or("SystemRoot unavailable")?;
    let shell = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    let mut command = CommandBuilder::new(shell);
    command.arg("/Q");
    command.arg("/D");
    command.cwd(&cwd);
    let mut child = pty
        .slave
        .spawn_command(command)
        .map_err(|e| format!("spawn owned shell: {e}"))?;
    drop(pty.slave);
    let result = (|| {
        let mut reader = pty.master.try_clone_reader().map_err(|e| e.to_string())?;
        let mut writer = pty.master.take_writer().map_err(|e| e.to_string())?;
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            let mut total = 0;
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                total += count;
                if total > 256 * 1024 {
                    break;
                }
                if sender.send(buffer[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        pty.master
            .resize(PtySize {
                rows: 31,
                cols: 103,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        let size = pty.master.get_size().map_err(|e| e.to_string())?;
        // cmd /D disables user AutoRun. chcp affects only this owned shell.
        writer
            .write_all(
                "chcp 65001\r\necho MULTICLI_S00_INPUT_OK\r\necho %CD%\r\nexit /b 0\r\n".as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        let status = loop {
            if std::time::Instant::now() >= deadline {
                return Err("owned shell PTY timed out".into());
            }
            while let Ok(chunk) = receiver.try_recv() {
                output.extend_from_slice(&chunk);
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        drop(writer);
        // The exited child closes the ConPTY output stream. Drain any final
        // output before joining, with a bounded wait instead of read_to_end.
        while let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(250)) {
            output.extend_from_slice(&chunk);
        }
        drop(receiver);
        drop(pty.master);
        if reader_thread.is_finished() {
            let _ = reader_thread.join();
        }
        let text = String::from_utf8_lossy(&output);
        Ok(PtySmoke {
            input_echo: text.contains("MULTICLI_S00_INPUT_OK"),
            unicode_cwd: text.contains(&cwd.display().to_string()),
            resize: size.rows == 31 && size.cols == 103,
            exit_success: status.success(),
        })
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[tauri::command]
async fn multicli_shell_smoke(runtime: State<'_, MultiCliRuntime>) -> Result<PtySmoke, String> {
    let root = runtime.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || shell_smoke(&root))
        .await
        .map_err(|e| e.to_string())?
}

fn webview_data_directory(root: &Path, profile: &str) -> PathBuf {
    let normal = root.join("webview");
    let valid_profile = matches!(profile, "dev" | "test" | "prod");
    if valid_profile && std::fs::create_dir_all(&normal).is_ok() {
        return normal;
    }
    // Fallback must preserve profile isolation, including simultaneous failed
    // opens. Never place a raw invalid environment value into a path.
    let safe_profile = if valid_profile { profile } else { "invalid" };
    let fallback = std::env::temp_dir()
        .join("MultiCLI")
        .join("webview-fallback")
        .join(safe_profile)
        .join(uuid::Uuid::new_v4().to_string());
    let _ = std::fs::create_dir_all(&fallback);
    fallback
}

pub fn run() {
    let runtime = MultiCliRuntime::from_environment();
    let root = runtime.root().to_path_buf();
    // A failed database open is surfaced to the UI as a real AppError and can
    // be retried from the window; startup never panics on it.
    let _ = runtime.service();
    // WebView data must live somewhere writable even when the database root is
    // currently invalid (for example a file blocking the directory). The UI
    // still receives the real database error and a retry action.
    let webview_directory = webview_data_directory(&root, runtime.profile());
    // No upstream plugins, startup migrations, history scan, network listener,
    // credential probing, CLI installation, updater or analytics is registered.
    tauri::Builder::default()
        .manage(runtime)
        .setup(move |app| {
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("MultiCLI — S01")
            .inner_size(1320.0, 820.0)
            .min_inner_size(960.0, 640.0)
            .data_directory(webview_directory)
            .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            multicli_baseline_info,
            multicli_shell_smoke,
            multicli_list_workspaces,
            multicli_list_agents,
            multicli_list_sessions,
            multicli_get_session,
            multicli_list_capabilities,
            multicli_export_diagnostics
        ])
        .run(tauri::generate_context!())
        .expect("run MultiCLI");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multicli::{
        AgentDefinition, CapabilityEvidenceLevel, CapabilityName, CapabilityStatus, Host, Message,
        Run, RunOwnership, RunTransport, Session, SessionOrigin, Workspace,
    };
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn failed_data_roots_keep_webview_profiles_separate() {
        let base = temp_root("fallback-profiles");
        let blocked = base.join("file");
        std::fs::write(&blocked, "keep").unwrap();
        let dev = webview_data_directory(&blocked, "dev");
        let test = webview_data_directory(&blocked, "test");
        let prod = webview_data_directory(&blocked, "prod");
        assert_ne!(dev, test);
        assert_ne!(test, prod);
        assert_ne!(dev, prod);
        let invalid = webview_data_directory(&blocked, "../../prod");
        assert!(invalid.starts_with(std::env::temp_dir().join("MultiCLI/webview-fallback/invalid")));
        assert_eq!(std::fs::read_to_string(&blocked).unwrap(), "keep");
    }

    #[test]
    fn actual_tauri_argument_errors_use_the_frozen_error_envelope() {
        let app = tauri::test::mock_builder()
            .manage(MultiCliRuntime::new(
                temp_root("ipc-wire-errors"),
                "test".into(),
            ))
            .invoke_handler(tauri::generate_handler![
                multicli_list_workspaces,
                multicli_list_agents,
                multicli_list_sessions,
                multicli_list_capabilities,
                multicli_get_session
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        for (cmd, body) in [
            ("multicli_list_workspaces", json!({})),
            (
                "multicli_list_workspaces",
                json!({"query":{"limit":"wrong"}}),
            ),
            ("multicli_list_agents", json!({"query":{"limit":0}})),
            (
                "multicli_list_sessions",
                json!({"query":{"limit":20,"unexpected":true}}),
            ),
            ("multicli_list_capabilities", json!({"query":{"limit":20}})),
            ("multicli_get_session", json!({"sessionId":42})),
        ] {
            let error = tauri::test::get_ipc_response(
                &window,
                tauri::webview::InvokeRequest {
                    cmd: cmd.into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: "http://tauri.localhost".parse().unwrap(),
                    body: tauri::ipc::InvokeBody::Json(body),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.into(),
                },
            )
            .expect_err("bad arguments must reject");
            assert_eq!(error["code"], "invalid_argument", "{cmd}: {error}");
            assert!(error["message"].is_string());
            assert!(error["retryable"].is_boolean());
            assert!(error["details"].is_object());
        }
    }

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "multicli-ipc-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn seed(service: &MultiCliService) {
        service.register_host(&Host::local("host-a")).unwrap();
        for (agent, display) in [
            ("agent-a", "Alpha"),
            ("agent-b", "Beta"),
            ("agent-c", "Gamma"),
        ] {
            service
                .register_agent(&AgentDefinition {
                    id: agent.into(),
                    vendor: "synthetic".into(),
                    display_name: display.into(),
                })
                .unwrap();
        }
        for (id, path) in [("ws-1", r"C:\Work\Alpha"), ("ws-2", r"C:\Work\Beta")] {
            service
                .register_workspace(&Workspace {
                    id: id.into(),
                    path: crate::multicli::HostPath {
                        host_id: "host-a".into(),
                        display_path: path.into(),
                        canonical_path: Some(path.into()),
                    },
                    repo_group_id: None,
                    worktree_id: None,
                })
                .unwrap();
        }
        service
            .ensure_session(&Session {
                id: "s-1".into(),
                agent_id: "agent-a".into(),
                host_id: "host-a".into(),
                source_namespace: "agent-a/synthetic/host-a".into(),
                native_id: Some("native-secret-id".into()),
                origin: SessionOrigin::Managed,
                title: "SENTINEL-SESSION-TITLE".into(),
            })
            .unwrap();
        service
            .attach_workspace(&crate::multicli::SessionWorkspace {
                session_id: "s-1".into(),
                workspace_id: "ws-1".into(),
                role: crate::multicli::SessionWorkspaceRole::Primary,
                evidence: "synthetic".into(),
            })
            .unwrap();
        service
            .attach_workspace(&crate::multicli::SessionWorkspace {
                session_id: "s-1".into(),
                workspace_id: "ws-2".into(),
                role: crate::multicli::SessionWorkspaceRole::Related,
                evidence: "synthetic".into(),
            })
            .unwrap();
        service
            .start_run(&Run {
                id: "r-1".into(),
                session_id: "s-1".into(),
                installation_id: None,
                transport: RunTransport::Native,
                ownership: RunOwnership::Managed,
                started_at: Some("2026-11-01T10:00:00.000Z".into()),
                ended_at: None,
                execution_status: crate::multicli::ExecutionStatus::Running,
                interaction_status: crate::multicli::InteractionStatus::Working,
                evidence_level: crate::multicli::EvidenceLevel::Observed,
                evidence: Some("synthetic".into()),
            })
            .unwrap();
        service
            .record_message(&Message {
                id: "m-1".into(),
                session_id: "s-1".into(),
                run_id: Some("r-1".into()),
                native_event_id: Some("native-event-secret".into()),
                source_key: "synthetic:m-1".into(),
                seq: 0,
                kind: "assistant-message".into(),
                content: json!({ "text": "SENTINEL-MESSAGE-BODY" }),
                source_ref: Some("synthetic".into()),
                observed_at: "2026-11-01T10:00:01.000Z".into(),
            })
            .unwrap();
        for (name, status, evidence) in [
            (
                CapabilityName::StructuredChat,
                CapabilityStatus::Supported,
                CapabilityEvidenceLevel::Live,
            ),
            (
                CapabilityName::QuotaRead,
                CapabilityStatus::Unsupported,
                CapabilityEvidenceLevel::Document,
            ),
            (
                CapabilityName::PermissionFlow,
                CapabilityStatus::Unverified,
                CapabilityEvidenceLevel::Fixture,
            ),
        ] {
            service
                .declare_capability(&Capability {
                    agent_id: "agent-a".into(),
                    installation_id: None,
                    name,
                    status,
                    version_range: Some(">=1".into()),
                    evidence_level: evidence,
                    source: "synthetic contract sample".into(),
                    verified_at: None,
                    reason: Some("fixture only".into()),
                })
                .unwrap();
        }
    }

    #[test]
    fn runtime_retries_a_failed_open_without_restarting() {
        let root = temp_root("retry");
        let blocked = root.join("multicli.db");
        std::fs::write(&blocked, b"not a database").unwrap();
        let runtime = MultiCliRuntime::new(root.clone(), "test".into());
        // A file with unrecognized content is refused as an unknown database.
        let error = match runtime.service() {
            Ok(_) => panic!("blocked root must fail"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::Database);
        assert!(error.retryable);
        std::fs::remove_file(&blocked).unwrap();
        let service = runtime.service().expect("retry opens the real database");
        assert!(service.database_path().exists());
        let conn = service.lock_for_test();
        let hosts: i64 = conn
            .query_row("SELECT COUNT(*) FROM hosts", [], |row| row.get(0))
            .unwrap();
        let sessions: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(hosts, 1, "startup must initialize only the local host");
        assert_eq!(sessions, 0, "startup must not fabricate sessions");
    }

    #[test]
    fn ipc_handlers_return_pages_real_errors_and_shared_session_detail() {
        let root = temp_root("handlers");
        let runtime = MultiCliRuntime::new(root, "test".into());
        let service = runtime.service().unwrap();
        seed(&service);

        let first = list_workspaces_impl(&service, &PageRequest::new(1, None)).unwrap();
        assert_eq!(first.total, 2);
        assert_eq!(first.items.len(), 1);
        let cursor = first.next_cursor.clone().expect("more workspaces");
        let second = list_workspaces_impl(&service, &PageRequest::new(1, Some(cursor))).unwrap();
        assert_eq!(second.total, 2);
        assert_ne!(first.items[0].workspace.id, second.items[0].workspace.id);
        assert_eq!(second.next_cursor, None);

        let agents = list_agents_impl(&service, &PageRequest::new(50, None)).unwrap();
        assert_eq!(agents.total, 3);
        assert_eq!(agents.items.len(), 3);

        let by_workspace = list_sessions_impl(
            &service,
            &SessionQuery {
                workspace_id: Some("ws-1".into()),
                agent_id: None,
                origin: None,
                limit: 50,
                cursor: None,
            },
        )
        .unwrap();
        let by_agent = list_sessions_impl(
            &service,
            &SessionQuery {
                workspace_id: None,
                agent_id: Some("agent-a".into()),
                origin: None,
                limit: 50,
                cursor: None,
            },
        )
        .unwrap();
        assert_eq!(by_workspace.total, 1);
        assert_eq!(by_agent.total, 1);
        assert_eq!(
            by_workspace.items[0].session.id,
            by_agent.items[0].session.id
        );
        assert_eq!(by_workspace.items[0].message_count, 1);
        assert_eq!(by_agent.items[0].message_count, 1);

        let detail = get_session_impl(&service, "s-1").unwrap();
        assert_eq!(detail.session.id, "s-1");
        assert_eq!(detail.message_count, 1);
        assert_eq!(detail.run_count, 1);
        assert_eq!(detail.workspaces.len(), 2);

        let missing = get_session_impl(&service, "missing").unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);
        let wire = serde_json::to_value(&missing).unwrap();
        for field in ["code", "message", "retryable", "details"] {
            assert!(wire.get(field).is_some(), "missing {field} in AppError");
        }
        assert_eq!(wire["code"], "not_found");

        let capabilities = list_capabilities_impl(
            &service,
            &CapabilityQuery {
                agent_id: "agent-a".into(),
                limit: 2,
                cursor: None,
            },
        )
        .unwrap();
        assert_eq!(capabilities.total, 3);
        assert_eq!(capabilities.items.len(), 2);
        let cursor = capabilities.next_cursor.clone().unwrap();
        let rest = list_capabilities_impl(
            &service,
            &CapabilityQuery {
                agent_id: "agent-a".into(),
                limit: 2,
                cursor: Some(cursor),
            },
        )
        .unwrap();
        assert_eq!(rest.total, 3);
        assert_eq!(rest.items.len(), 1);
        assert_eq!(rest.next_cursor, None);

        let invalid = list_workspaces_impl(&service, &PageRequest::new(0, None)).unwrap_err();
        assert_eq!(invalid.code, ErrorCode::InvalidArgument);
        let invalid = list_workspaces_impl(&service, &PageRequest::new(201, None)).unwrap_err();
        assert_eq!(invalid.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn diagnostics_export_uses_whitelist_and_reports_real_counts() {
        let root = temp_root("diagnostics");
        let runtime = MultiCliRuntime::new(root.clone(), "test".into());
        let service = runtime.service().unwrap();
        seed(&service);
        let export = export_diagnostics_impl(&service, "test").unwrap();
        assert_eq!(export.created_at.ends_with('Z'), true);
        let path = PathBuf::from(&export.path);
        assert!(path.starts_with(root.join("diagnostics")));
        let text = std::fs::read_to_string(&path).unwrap();
        let document: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(document["schemaVersion"], 1);
        assert_eq!(document["profile"], "test");
        assert_eq!(document["counts"]["agents"], 3);
        assert_eq!(document["counts"]["workspaces"], 2);
        assert_eq!(document["counts"]["sessions"], 1);
        assert_eq!(document["counts"]["messages"], 1);
        assert_eq!(document["counts"]["runs"], 1);
        assert_eq!(document["database"]["schemaVersion"], 2);
        assert_eq!(document["database"]["journalMode"], "wal");
        let agent = &document["capabilities"]["byAgent"][0];
        assert_eq!(agent["agentId"], "agent-1");
        assert_eq!(agent["supported"], 1);
        assert_eq!(agent["unsupported"], 1);
        assert_eq!(agent["unverified"], 1);
        assert_eq!(agent["document"], 1);
        assert_eq!(agent["fixture"], 1);
        assert_eq!(agent["live"], 1);

        for forbidden in [
            "SENTINEL-SESSION-TITLE",
            "SENTINEL-MESSAGE-BODY",
            "native-secret-id",
            "native-event-secret",
            "C:\\Work",
            root.display().to_string().as_str(),
            "token",
            "cookie",
        ] {
            assert!(
                !text.contains(forbidden),
                "diagnostics leaked forbidden value: {forbidden}"
            );
        }
    }

    #[test]
    fn diagnostics_failure_is_an_error_not_an_empty_success() {
        let root = temp_root("diagnostics-failure");
        let runtime = MultiCliRuntime::new(root.clone(), "test".into());
        let service = runtime.service().unwrap();
        std::fs::write(root.join("diagnostics"), b"blocked").unwrap();
        let error = export_diagnostics_impl(&service, "test").unwrap_err();
        assert_eq!(error.code, ErrorCode::Io);
        assert!(error.retryable);
        assert!(matches!(
            error.details.get("stage").and_then(|v| v.as_str()),
            Some("exportDiagnostics")
        ));
    }

    #[test]
    fn diagnostics_unavailable_database_returns_error_not_zero_counts() {
        let root = temp_root("diagnostics-unavailable");
        std::fs::write(root.join("multicli.db"), b"not a database").unwrap();
        let runtime = MultiCliRuntime::new(root.clone(), "test".into());
        let error = match runtime.service() {
            Ok(_) => panic!("blocked database must not open"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::Database);
        assert!(error.retryable);
        assert!(
            !root.join("diagnostics").exists(),
            "diagnostics must not be written when the database is unavailable"
        );
    }

    #[test]
    fn ipc_query_wire_shape_is_camel_case_bounded_and_closed() {
        let workspaces: PageRequest = serde_json::from_value(json!({ "limit": 50 })).unwrap();
        assert_eq!(workspaces.limit, 50);
        assert!(workspaces.cursor.is_none());

        let sessions: SessionQuery = serde_json::from_value(json!({
            "workspaceId": "ws-1",
            "agentId": "agent-a",
            "origin": "managed",
            "limit": 20,
            "cursor": "abc",
        }))
        .unwrap();
        assert_eq!(sessions.workspace_id.as_deref(), Some("ws-1"));
        assert_eq!(sessions.agent_id.as_deref(), Some("agent-a"));
        assert!(sessions.validate().is_ok());

        let capabilities: CapabilityQuery =
            serde_json::from_value(json!({ "agentId": "agent-a", "limit": 200 })).unwrap();
        assert!(capabilities.validate().is_ok());

        assert!(
            serde_json::from_value::<PageRequest>(json!({ "limit": 1, "extra": true })).is_err()
        );
        assert!(serde_json::from_value::<PageRequest>(json!({ "limit": 0 }))
            .unwrap()
            .validate()
            .is_err());
        assert!(
            serde_json::from_value::<PageRequest>(json!({ "limit": 201 }))
                .unwrap()
                .validate()
                .is_err()
        );
        assert!(
            serde_json::from_value::<CapabilityQuery>(json!({ "agentId": " ", "limit": 10 }))
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn profile_root_is_outside_upstream_data() {
        let root = data_root().unwrap();
        assert!(root.is_absolute());
        assert!(root.starts_with(dirs::data_local_dir().unwrap().join("MultiCLI")));
        assert!(!root.starts_with(crate::paths::home_dir().join(".ccgui-next")));
    }

    #[test]
    fn data_profiles_are_disjoint_and_reject_paths() {
        let base = std::env::temp_dir().join("multicli-synthetic-root");
        let dev = data_root_for(&base, "dev").unwrap();
        let test = data_root_for(&base, "test").unwrap();
        let prod = data_root_for(&base, "prod").unwrap();
        assert_ne!(dev, test);
        assert_ne!(test, prod);
        assert_ne!(dev, prod);
        for bad in ["", "../.ccgui-next", "..\\.codex", "PROD", "C:\\Users"] {
            assert!(
                data_root_for(&base, bad).is_err(),
                "invalid profile accepted: {bad}"
            );
        }
        assert!(data_root_for(std::path::Path::new("relative"), "test").is_err());
        assert_eq!(selected_profile(None, true).unwrap(), "dev");
        assert_eq!(selected_profile(None, false).unwrap(), "prod");
    }

    #[cfg(windows)]
    #[test]
    fn real_windows_shell_round_trip() {
        let result = shell_smoke(&std::env::temp_dir().join("multicli-synthetic-root"))
            .expect("real Windows PTY smoke");
        assert!(result.input_echo, "shell did not execute typed echo");
        assert!(
            result.unicode_cwd,
            "shell cwd did not preserve Chinese/space path"
        );
        assert!(result.resize, "PTY geometry did not change");
        assert!(result.exit_success, "owned shell did not exit successfully");
    }
}
