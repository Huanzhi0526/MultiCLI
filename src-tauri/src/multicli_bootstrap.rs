//! MultiCLI startup boundary and product IPC registration.
//!
//! Upstream startup is retained for reference but never invoked: its
//! migrations, CLI skill installation and background hosts do not belong to
//! this application. This file is also the only place that registers product
//! commands; the domain module stays transport-independent.
use crate::multicli::discovery::detect::ScannerEnvironment;
use crate::multicli::discovery::winproc::WindowsProcessBackend;
use crate::multicli::discovery::{
    DiscoveryEngine, DiscoverySettings, DiscoverySettingsUpdate, InstallationDiscoveryView,
    InstallationQuery, ProcessDiscoveryPage, ProcessQuery, ScanView,
};
use crate::multicli::history::{
    HistoryCandidateView, HistoryEngine, HistoryOverallStatus, HistoryRebuildQuery,
    HistoryRefreshQuery, HistorySearchQuery, HistorySourceUpdate, HistorySourceView, MessagePage,
    MessagePageQuery, ResumeTargetView, SearchPage, SessionMetadataView,
};
use crate::multicli::runtime::{
    RuntimeAnswerRequest, RuntimeCancelRequest, RuntimeManager, RuntimeNotifyFn, RuntimeReadRequest,
    RuntimeResumeRequest, RuntimeSendRequest, RuntimeStartRequest, RuntimeStopRequest,
};
use crate::multicli::usage::{
    UsageCenter, UsageOverviewView, UsageRefreshRequest, UsageRefreshResult,
};
use crate::multicli::terminal::{
    NotifyFn as TerminalNotifyFn, TerminalCloseRequest, TerminalCloseView, TerminalLimits,
    TerminalLimitsUpdate, TerminalListView, TerminalManager, TerminalNotify, TerminalOpenSpec,
    TerminalQuitCheckView, TerminalReadRequest, TerminalReadView, TerminalResizeRequest,
    TerminalResizeView, TerminalShutdownAllRequest, TerminalShutdownAllView, TerminalStopMode,
    TerminalStopRequest, TerminalStopView, TerminalTargetsView, TerminalWriteRequest,
    TerminalWriteView,
};
use crate::multicli::{
    AgentListItem, AppError, Capability, CapabilityQuery, CustomCliInput, CustomCliView,
    DiagnosticsExport, ErrorCode, Host, MultiCliService, Page, PageRequest, SessionDetail,
    SessionQuery, SessionSummary, WorkspaceListItem,
};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri::State;

/// Frontend notification for a managed terminal. The payload is a few bytes:
/// the actual output is pulled with `multicli_terminal_read`, so a slow WebView
/// can never build an unbounded IPC queue.
const TERMINAL_EVENT: &str = "multicli://terminal";
/// Asked by the tray when the user picks "退出"; the window decides between
/// cancelling and stopping this application's own terminals.
const QUIT_REQUESTED_EVENT: &str = "multicli://quit-requested";

/// Structured-runtime notification. Like the terminal one it carries only a few
/// bytes; the events themselves are pulled with `multicli_runtime_read`.
const RUNTIME_EVENT: &str = "multicli://runtime";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselineInfo {
    product: &'static str,
    stage: &'static str,
    profile: String,
    data_root: String,
    data_root_override: bool,
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
    // Verification and portable runs may pin the exact profile directory.
    // The default stays the Windows known-folder path; an override is opt-in,
    // must be absolute, and is reported in the baseline view so nobody has to
    // guess which database a window is using.
    if let Some(raw) = std::env::var_os("MULTICLI_DATA_ROOT") {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err("MULTICLI_DATA_ROOT must be an absolute path".into());
        }
        return Ok(path);
    }
    let base = dirs::data_local_dir().ok_or("Local application data directory unavailable")?;
    data_root_for(&base, &profile()?)
}

/// `true` when the current process pinned its data root via the environment.
pub(crate) fn data_root_override() -> bool {
    std::env::var_os("MULTICLI_DATA_ROOT").is_some()
}

/// Holds the lazily opened service. A failed open is remembered as a real
/// `AppError` and retried on the next command, so the UI can offer a retry
/// action instead of showing an empty database or crashing the process.
pub struct MultiCliRuntime {
    root: PathBuf,
    profile: String,
    state: Mutex<RuntimeState>,
    /// A planned restore is a startup action, not a retry action. Once this
    /// process has attempted it, an IPC retry or a data-page refresh must not
    /// replace a database that is already open.
    restore_attempted: std::sync::atomic::AtomicBool,
    discovery: Mutex<Option<Arc<DiscoveryEngine>>>,
    history: Mutex<Option<Arc<HistoryEngine>>>,
    terminal: Mutex<Option<Arc<TerminalManager>>>,
    runtime: Mutex<Option<Arc<RuntimeManager>>>,
    usage: Mutex<Option<Arc<UsageCenter>>>,
    /// Stops the permission sweeper when the application exits. It only ever
    /// expires this application's own requests.
    runtime_sweeper: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
    /// Set once in `setup`; the terminal manager emits through it. A missing
    /// handle is a real error, never a silently dropped notification.
    app: Mutex<Option<tauri::AppHandle>>,
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
            restore_attempted: std::sync::atomic::AtomicBool::new(false),
            discovery: Mutex::new(None),
            history: Mutex::new(None),
            terminal: Mutex::new(None),
            runtime: Mutex::new(None),
            usage: Mutex::new(None),
            runtime_sweeper: Mutex::new(None),
            app: Mutex::new(None),
        }
    }

    /// True for the first call in this process only. A retry or a data-page
    /// refresh must not apply a planned restore to an already-open database.
    fn take_restore_attempt(&self) -> bool {
        !self
            .restore_attempted
            .swap(true, std::sync::atomic::Ordering::SeqCst)
    }

    /// Called once from `setup`, before the first terminal command can run.
    pub fn attach_app(&self, app: tauri::AppHandle) {
        *self.app.lock() = Some(app);
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
        // A planned restore is applied **before** the database is opened, so
        // no connection ever points at a file that is being replaced. A
        // failure keeps the marker and the original database; the data view
        // shows that the restore did not run.
        if self.take_restore_attempt() {
            if let Err(error) = crate::multicli::backup::apply_pending_restore(&root) {
                eprintln!(
                    "[multicli] planned profile restore was not applied: {}: {}",
                    error.code.as_str(),
                    error.message
                );
                // Keep the marker, but mark it failed so the data page can show a
                // real state instead of claiming the restore is still pending.
                let _ = crate::multicli::backup::mark_restore_failed(&root, &error);
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
    /// Lazily creates the discovery engine. The engine opens no database of its
    /// own; it reuses the same service and data root and starts one background
    /// sampling thread. A failed database open is returned as the same real
    /// `AppError` the rest of the UI already shows.
    pub fn discovery(&self) -> Result<Arc<DiscoveryEngine>, AppError> {
        let mut slot = self.discovery.lock();
        if let Some(engine) = slot.as_ref() {
            return Ok(engine.clone());
        }
        let service = self.service()?;
        let host_id = "local-windows";
        let engine = Arc::new(DiscoveryEngine::new(
            host_id,
            &self.root,
            service,
            Arc::new(WindowsProcessBackend::new(host_id)),
            ScannerEnvironment::from_process_env(),
        ));
        engine.start();
        *slot = Some(engine.clone());
        Ok(engine)
    }

    /// Lazily creates the history indexer. It reuses the same service and data
    /// root, opens no second database and starts one background thread that
    /// only reads the configured roots.
    pub fn history(&self) -> Result<Arc<HistoryEngine>, AppError> {
        let mut slot = self.history.lock();
        if let Some(engine) = slot.as_ref() {
            return Ok(engine.clone());
        }
        let service = self.service()?;
        let engine = Arc::new(HistoryEngine::new("local-windows", service));
        engine.start();
        *slot = Some(engine.clone());
        Ok(engine)
    }

    /// Ends only the application's own sampling thread. External CLI processes
    /// are untouched.
    pub fn stop_discovery(&self) {
        let slot = self.discovery.lock();
        if let Some(engine) = slot.as_ref() {
            engine.stop();
        }
    }

    /// Ends only the application's own indexing thread.
    pub fn stop_history(&self) {
        let slot = self.history.lock();
        if let Some(engine) = slot.as_ref() {
            engine.stop();
        }
    }

    /// Lazily creates the managed terminal registry. It shares the same
    /// database and data root, writes logs only under this profile and emits
    /// one small notification per batch of output.
    pub fn terminal(&self) -> Result<Arc<TerminalManager>, AppError> {
        let mut slot = self.terminal.lock();
        if let Some(manager) = slot.as_ref() {
            return Ok(manager.clone());
        }
        let service = self.service()?;
        let app = self.app.lock().clone().ok_or_else(|| {
            AppError::new(
                ErrorCode::Io,
                "the application handle is not attached; the terminal cannot report events",
            )
            .with_details(serde_json::json!({ "stage": "attachApp" }))
            .with_retryable(true)
        })?;
        let notifier: TerminalNotifyFn = Arc::new(move |payload: TerminalNotify| {
            use tauri::Emitter;
            let _ = app.emit(TERMINAL_EVENT, &payload);
        });
        let manager = Arc::new(TerminalManager::new(
            service,
            self.root.clone(),
            self.profile.clone(),
            notifier,
        )?);
        *slot = Some(manager.clone());
        Ok(manager)
    }

    /// Lazily creates the structured runtime registry. It shares the same
    /// database and data root, writes only under this profile and emits one
    /// small notification per state change.
    pub fn runtime(&self) -> Result<Arc<RuntimeManager>, AppError> {
        let mut slot = self.runtime.lock();
        if let Some(manager) = slot.as_ref() {
            return Ok(manager.clone());
        }
        let service = self.service()?;
        let app = self.app.lock().clone().ok_or_else(|| {
            AppError::new(
                ErrorCode::Io,
                "the application handle is not attached; the runtime cannot report events",
            )
            .with_details(serde_json::json!({ "stage": "attachApp" }))
            .with_retryable(true)
        })?;
        let notifier: RuntimeNotifyFn = Arc::new(move |payload| {
            use tauri::Emitter;
            let _ = app.emit(RUNTIME_EVENT, &payload);
        });
        let manager = Arc::new(RuntimeManager::new(
            service,
            self.root.clone(),
            self.profile.clone(),
            notifier,
        )?);
        // One sweeper for every run: a permission request that nobody answers
        // must become `expired` instead of spinning forever. It only ever
        // answers with the request's own deny option.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sweep_manager = Arc::clone(&manager);
        let sweep_stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("multicli-runtime-sweeper".into())
            .spawn(move || {
                while !sweep_stop.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1_000));
                    if sweep_stop.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    sweep_manager.sweep_permissions();
                }
            })
            .map_err(|error| {
                AppError::new(
                    ErrorCode::Io,
                    format!("failed to start the runtime sweeper: {error}"),
                )
                .with_details(serde_json::json!({ "stage": "runtimeSweeper" }))
            })?;
        *self.runtime_sweeper.lock() = Some(stop);
        *slot = Some(manager.clone());
        Ok(manager)
    }

    /// Lazily creates the usage centre. It shares the same database and data
    /// root; the only process it ever starts is its own short-lived
    /// `codex app-server` read, which it also closes.
    pub fn usage(&self) -> Result<Arc<UsageCenter>, AppError> {
        let mut slot = self.usage.lock();
        if let Some(center) = slot.as_ref() {
            return Ok(center.clone());
        }
        let service = self.service()?;
        let center = Arc::new(UsageCenter::new(
            "local-windows",
            service,
            self.root.clone(),
        ));
        *slot = Some(center.clone());
        Ok(center)
    }

    // -----------------------------------------------------------------------
    // Own-profile backup / restore (S07)
    // -----------------------------------------------------------------------

    /// Writes one consistent snapshot of this profile's database. Only the
    /// MultiCLI database is exported; CLI credentials and transcripts are not
    /// part of this profile.
    pub fn backup_profile(&self) -> Result<crate::multicli::backup::BackupRecord, AppError> {
        let service = self.service()?;
        // A dedicated read-only connection keeps the global database mutex free
        // while VACUUM INTO and the checksum run. `VACUUM INTO` is a read
        // operation on the source, so this is safe with WAL enabled.
        let connection = rusqlite::Connection::open_with_flags(
            service.database_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|error| {
            AppError::new(ErrorCode::Io, format!("backup connection failed: {error}"))
                .with_details(serde_json::json!({ "stage": "backupConnection" }))
                .with_retryable(true)
        })?;
        crate::multicli::backup::create_backup(
            &connection,
            &self.root,
            &self.profile,
            env!("CARGO_PKG_VERSION"),
            "manual",
        )
    }

    pub fn profile_data(&self) -> Result<crate::multicli::backup::ProfileDataView, AppError> {
        // This page is also the recovery entry point. A damaged or exclusively
        // locked database must not hide the backup list, so opening the store
        // is best-effort: schema/journal simply report what the file says.
        let journal = match self.service() {
            Ok(service) => service
                .journal_mode()
                .unwrap_or_else(|_| "unknown".to_string()),
            Err(_) => "unknown".to_string(),
        };
        crate::multicli::backup::profile_view(
            &self.root,
            &self.profile,
            &journal,
            crate::multicli::SCHEMA_VERSION,
        )
    }

    pub fn plan_profile_restore(
        &self,
        file_name: &str,
    ) -> Result<crate::multicli::backup::PendingRestore, AppError> {
        crate::multicli::backup::plan_restore(&self.root, file_name)
    }

    pub fn cancel_profile_restore(&self) -> Result<(), AppError> {
        crate::multicli::backup::cancel_pending_restore(&self.root)
    }

    /// Ends every owned protocol connection. Called from the application exit
    /// event; external CLI processes are never enumerated.
    pub fn stop_runtime(&self) {
        if let Some(stop) = self.runtime_sweeper.lock().as_ref() {
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if let Some(manager) = self.runtime.lock().as_ref() {
            manager.shutdown_all();
        }
    }

    /// Ends every owned terminal tree. Called from the application exit event
    /// so a crash-free exit never leaves an orphan family; external processes
    /// are not enumerated at all.
    pub fn stop_terminals(&self) {
        if let Some(manager) = self.terminal.lock().as_ref() {
            manager.hard_shutdown_all();
        }
    }
}

fn usage_overview_impl(center: &UsageCenter) -> Result<UsageOverviewView, AppError> {
    center.overview()
}

// ---------------------------------------------------------------------------
// Own-profile data: backup, planned restore, profile facts
// ---------------------------------------------------------------------------

#[tauri::command]
async fn multicli_profile_data(
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::backup::ProfileDataView, AppError> {
    runtime.profile_data()
}

#[tauri::command]
async fn multicli_profile_backup(
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::backup::BackupRecord, AppError> {
    runtime.backup_profile()
}

#[tauri::command]
async fn multicli_profile_restore(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::backup::PendingRestore, AppError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct RestoreQuery {
        file_name: String,
    }
    let query: RestoreQuery = required_argument(query, "query")?;
    runtime.plan_profile_restore(&query.file_name)
}

#[tauri::command]
async fn multicli_profile_cancel_restore(
    runtime: State<'_, MultiCliRuntime>,
) -> Result<(), AppError> {
    runtime.cancel_profile_restore()
}

// A usage read can start a CLI and wait for its answer. Running it as an async
// command keeps that wait off the window's main thread; the UI stays
// responsive while the numbers are being read.
#[tauri::command]
async fn multicli_usage_overview(
    runtime: State<'_, MultiCliRuntime>,
) -> Result<UsageOverviewView, AppError> {
    let center = runtime.usage()?;
    usage_overview_impl(&center)
}

fn usage_refresh_impl(
    center: &UsageCenter,
    request: &UsageRefreshRequest,
) -> Result<UsageRefreshResult, AppError> {
    center.refresh(request)
}

#[tauri::command]
async fn multicli_usage_refresh(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<UsageRefreshResult, AppError> {
    let request: UsageRefreshRequest = match query {
        Some(value) => serde_json::from_value(value)
            .map_err(|error| AppError::invalid_argument("query", error.to_string()))?,
        None => UsageRefreshRequest {
            agent_id: None,
            force: false,
        },
    };
    let center = runtime.usage()?;
    usage_refresh_impl(&center, &request)
}

#[tauri::command]
fn multicli_custom_cli_list(runtime: State<'_, MultiCliRuntime>) -> Result<Vec<CustomCliView>, AppError> {
    let service = runtime.service()?;
    service.list_custom_clis("local-windows")
}

#[tauri::command]
fn multicli_custom_cli_create(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<CustomCliView, AppError> {
    let input: CustomCliInput = required_argument(query, "query")?;
    let service = runtime.service()?;
    service.create_custom_cli("local-windows", &input)
}

#[tauri::command]
fn multicli_custom_cli_delete(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<(), AppError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct DeleteQuery {
        id: String,
    }
    let query: DeleteQuery = required_argument(query, "query")?;
    let service = runtime.service()?;
    service.delete_custom_cli("local-windows", &query.id)
}

#[tauri::command]
fn multicli_baseline_info(runtime: State<'_, MultiCliRuntime>) -> Result<BaselineInfo, String> {
    Ok(BaselineInfo {
        product: "MultiCLI",
        stage: "local Windows CLI workbench",
        profile: runtime.profile().to_string(),
        data_root: runtime.root().display().to_string(),
        data_root_override: data_root_override(),
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

fn list_installations_impl(
    engine: &DiscoveryEngine,
    query: &InstallationQuery,
) -> Result<Page<InstallationDiscoveryView>, AppError> {
    engine.list_installations(query)
}

#[tauri::command]
fn multicli_list_installations(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<Page<InstallationDiscoveryView>, AppError> {
    let query: InstallationQuery = required_argument(query, "query")?;
    query.validate()?;
    let engine = runtime.discovery()?;
    list_installations_impl(&engine, &query)
}

fn list_processes_impl(
    engine: &DiscoveryEngine,
    query: &ProcessQuery,
) -> Result<ProcessDiscoveryPage, AppError> {
    engine.list_processes(query)
}

#[tauri::command]
fn multicli_list_processes(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<ProcessDiscoveryPage, AppError> {
    let query: ProcessQuery = required_argument(query, "query")?;
    query.validate()?;
    let engine = runtime.discovery()?;
    list_processes_impl(&engine, &query)
}

fn refresh_discovery_impl(engine: &DiscoveryEngine) -> ScanView {
    engine.refresh()
}

/// Manual refresh. Shares the in-flight scan instead of stacking scanners and
/// returns the generation that actually completed.
#[tauri::command]
fn multicli_refresh_discovery(runtime: State<'_, MultiCliRuntime>) -> Result<ScanView, AppError> {
    let engine = runtime.discovery()?;
    Ok(refresh_discovery_impl(&engine))
}

fn update_discovery_settings_impl(
    engine: &DiscoveryEngine,
    query: &DiscoverySettingsUpdate,
) -> Result<DiscoverySettings, AppError> {
    engine.update_settings(query)
}

/// Bounded scan-interval setting. It is persisted only under this profile's
/// own data root (`discovery-settings.json`) and never touches a CLI config.
#[tauri::command]
fn multicli_update_discovery_settings(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<DiscoverySettings, AppError> {
    let query: DiscoverySettingsUpdate = required_argument(query, "query")?;
    if !(crate::multicli::discovery::view::MIN_SCAN_INTERVAL_MS
        ..=crate::multicli::discovery::view::MAX_SCAN_INTERVAL_MS)
        .contains(&query.interval_ms)
    {
        return Err(AppError::invalid_argument(
            "intervalMs",
            "must be between 1000 and 60000",
        ));
    }
    let engine = runtime.discovery()?;
    update_discovery_settings_impl(&engine, &query)
}

// ---------------------------------------------------------------------------
// S03 history commands
// ---------------------------------------------------------------------------

fn list_history_sources_impl(
    service: &MultiCliService,
    query: &PageRequest,
) -> Result<Page<HistorySourceView>, AppError> {
    crate::multicli::history::query::list_sources_page(service, query)
}

#[tauri::command]
fn multicli_list_history_sources(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<Page<HistorySourceView>, AppError> {
    let query: PageRequest = required_argument(query, "query")?;
    query.validate()?;
    let service = runtime.service()?;
    list_history_sources_impl(&service, &query)
}

fn list_history_candidates_impl(
    service: &MultiCliService,
) -> Result<Vec<HistoryCandidateView>, AppError> {
    crate::multicli::history::query::list_candidates(service)
}

/// Known product roots under the user's home. Read-only metadata: a bounded
/// top-level listing, never a recursive walk and never a file read.
#[tauri::command]
fn multicli_list_history_candidates(
    runtime: State<'_, MultiCliRuntime>,
) -> Result<Vec<HistoryCandidateView>, AppError> {
    let service = runtime.service()?;
    list_history_candidates_impl(&service)
}

fn update_history_source_impl(
    engine: &HistoryEngine,
    query: &HistorySourceUpdate,
) -> Result<HistorySourceView, AppError> {
    engine.update_source(query)
}

#[tauri::command]
fn multicli_update_history_source(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<HistorySourceView, AppError> {
    let query: HistorySourceUpdate = required_argument(query, "query")?;
    query.validate()?;
    let engine = runtime.history()?;
    update_history_source_impl(&engine, &query)
}

fn refresh_history_impl(
    engine: &HistoryEngine,
    query: &HistoryRefreshQuery,
) -> Result<HistoryOverallStatus, AppError> {
    engine.refresh(query)
}

#[tauri::command]
fn multicli_refresh_history(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<HistoryOverallStatus, AppError> {
    let query: HistoryRefreshQuery = required_argument(query, "query")?;
    query.validate()?;
    let engine = runtime.history()?;
    refresh_history_impl(&engine, &query)
}

fn history_status_impl(engine: &HistoryEngine) -> Result<HistoryOverallStatus, AppError> {
    engine.status()
}

#[tauri::command]
fn multicli_get_history_status(
    runtime: State<'_, MultiCliRuntime>,
) -> Result<HistoryOverallStatus, AppError> {
    let engine = runtime.history()?;
    history_status_impl(&engine)
}

fn rebuild_history_impl(
    engine: &HistoryEngine,
    query: &HistoryRebuildQuery,
) -> Result<HistoryOverallStatus, AppError> {
    engine.rebuild(query)
}

#[tauri::command]
fn multicli_rebuild_history_index(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<HistoryOverallStatus, AppError> {
    let query: HistoryRebuildQuery = required_argument(query, "query")?;
    query.validate()?;
    let engine = runtime.history()?;
    rebuild_history_impl(&engine, &query)
}

fn session_metadata_impl(
    service: &MultiCliService,
    session_id: &str,
) -> Result<SessionMetadataView, AppError> {
    crate::multicli::history::query::session_metadata(service, session_id)
}

#[tauri::command]
fn multicli_get_session_metadata(
    session_id: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<SessionMetadataView, AppError> {
    let session_id: String = required_argument(session_id, "sessionId")?;
    let service = runtime.service()?;
    session_metadata_impl(&service, &session_id)
}

fn session_messages_impl(
    service: &MultiCliService,
    query: &MessagePageQuery,
) -> Result<MessagePage, AppError> {
    crate::multicli::history::query::message_page(service, query)
}

#[tauri::command]
fn multicli_list_session_messages(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<MessagePage, AppError> {
    let query: MessagePageQuery = required_argument(query, "query")?;
    query.validate()?;
    let service = runtime.service()?;
    session_messages_impl(&service, &query)
}

fn search_history_impl(
    service: &MultiCliService,
    query: &HistorySearchQuery,
) -> Result<SearchPage, AppError> {
    crate::multicli::history::query::search_history(service, query)
}

#[tauri::command]
fn multicli_search_history(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<SearchPage, AppError> {
    let query: HistorySearchQuery = required_argument(query, "query")?;
    query.validate()?;
    let service = runtime.service()?;
    search_history_impl(&service, &query)
}

fn resolve_resume_target_impl(
    service: &MultiCliService,
    session_id: &str,
) -> Result<ResumeTargetView, AppError> {
    crate::multicli::history::resume::resolve_resume_target(service, session_id)
}

/// Read-only target resolution. It never starts a process, writes a CLI file or
/// creates a Run; actual resume execution is S05.
#[tauri::command]
fn multicli_resolve_resume_target(
    session_id: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<ResumeTargetView, AppError> {
    let session_id: String = required_argument(session_id, "sessionId")?;
    let service = runtime.service()?;
    resolve_resume_target_impl(&service, &session_id)
}

// ---------------------------------------------------------------------------
// S04 managed terminal commands
// ---------------------------------------------------------------------------

fn blocking_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        ErrorCode::Io,
        format!("the terminal worker did not finish: {error}"),
    )
    .with_details(serde_json::json!({ "stage": "terminalWorker" }))
    .with_retryable(true)
}

#[tauri::command]
fn multicli_terminal_targets(
    runtime: State<'_, MultiCliRuntime>,
) -> Result<TerminalTargetsView, AppError> {
    runtime.terminal()?.targets()
}

#[tauri::command]
async fn multicli_terminal_open(
    spec: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::terminal::TerminalView, AppError> {
    let spec: TerminalOpenSpec = required_argument(spec, "spec")?;
    let manager = runtime.terminal()?;
    tauri::async_runtime::spawn_blocking(move || manager.open(&spec))
        .await
        .map_err(blocking_error)?
}

#[tauri::command]
fn multicli_terminal_list(
    runtime: State<'_, MultiCliRuntime>,
) -> Result<TerminalListView, AppError> {
    Ok(runtime.terminal()?.list())
}

#[tauri::command]
fn multicli_terminal_read(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<TerminalReadView, AppError> {
    let query: TerminalReadRequest = required_argument(query, "query")?;
    runtime.terminal()?.read(&query)
}

#[tauri::command]
async fn multicli_terminal_write(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<TerminalWriteView, AppError> {
    let query: TerminalWriteRequest = required_argument(query, "query")?;
    let manager = runtime.terminal()?;
    tauri::async_runtime::spawn_blocking(move || manager.write(&query))
        .await
        .map_err(blocking_error)?
}

#[tauri::command]
fn multicli_terminal_resize(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<TerminalResizeView, AppError> {
    let query: TerminalResizeRequest = required_argument(query, "query")?;
    runtime.terminal()?.resize(&query)
}

#[tauri::command]
async fn multicli_terminal_stop(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<TerminalStopView, AppError> {
    let query: TerminalStopRequest = required_argument(query, "query")?;
    let manager = runtime.terminal()?;
    tauri::async_runtime::spawn_blocking(move || manager.stop(&query))
        .await
        .map_err(blocking_error)?
}

#[tauri::command]
async fn multicli_terminal_close(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<TerminalCloseView, AppError> {
    let query: TerminalCloseRequest = required_argument(query, "query")?;
    let manager = runtime.terminal()?;
    tauri::async_runtime::spawn_blocking(move || manager.close(&query))
        .await
        .map_err(blocking_error)?
}

#[tauri::command]
async fn multicli_terminal_shutdown_all(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<TerminalShutdownAllView, AppError> {
    let query: TerminalShutdownAllRequest = match query {
        Some(value) => serde_json::from_value(value)
            .map_err(|error| AppError::invalid_argument("query", error.to_string()))?,
        None => TerminalShutdownAllRequest {
            mode: Some(TerminalStopMode::Gentle),
            grace_ms: None,
        },
    };
    let manager = runtime.terminal()?;
    tauri::async_runtime::spawn_blocking(move || manager.shutdown_all(&query))
        .await
        .map_err(blocking_error)?
}

#[tauri::command]
fn multicli_terminal_limits(runtime: State<'_, MultiCliRuntime>) -> Result<TerminalLimits, AppError> {
    Ok(runtime.terminal()?.limits())
}

#[tauri::command]
fn multicli_terminal_update_limits(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<TerminalLimits, AppError> {
    let query: TerminalLimitsUpdate = required_argument(query, "query")?;
    runtime.terminal()?.update_limits(&query)
}

#[tauri::command]
fn multicli_terminal_quit_check(
    runtime: State<'_, MultiCliRuntime>,
) -> Result<TerminalQuitCheckView, AppError> {
    Ok(runtime.terminal()?.quit_check())
}

/// Explicit application exit. `confirm` must be `true`: the UI has already
/// shown the choice, so there is no path where quitting silently kills a
/// terminal. Only this application's own trees are stopped; external processes
/// are never enumerated.
#[tauri::command]
async fn multicli_app_quit(
    confirm: Option<bool>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<(), AppError> {
    if confirm != Some(true) {
        return Err(AppError::invalid_argument(
            "confirm",
            "must be true; quitting is never implicit",
        ));
    }
    runtime.stop_runtime();
    if let Ok(manager) = runtime.terminal() {
        let request = TerminalShutdownAllRequest {
            mode: Some(TerminalStopMode::Gentle),
            grace_ms: Some(2_000),
        };
        tauri::async_runtime::spawn_blocking(move || manager.shutdown_all(&request))
            .await
            .map_err(blocking_error)??;
    }
    let app = runtime.app.lock().clone();
    if let Some(app) = app {
        app.exit(0);
        return Ok(());
    }
    Err(AppError::new(
        ErrorCode::Io,
        "the application handle is not attached",
    )
    .with_details(serde_json::json!({ "stage": "quit" })))
}

// ---------------------------------------------------------------------------
// S05 structured runtime commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn multicli_runtime_capabilities(
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::runtime::RuntimeCapabilitiesView, AppError> {
    runtime.runtime()?.capabilities()
}

#[tauri::command]
async fn multicli_runtime_start(
    spec: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::runtime::RuntimeRunView, AppError> {
    let spec: RuntimeStartRequest = required_argument(spec, "spec")?;
    spec.validate()?;
    let manager = runtime.runtime()?;
    tauri::async_runtime::spawn_blocking(move || manager.start(&spec))
        .await
        .map_err(blocking_error)?
}

#[tauri::command]
async fn multicli_runtime_resume(
    spec: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::runtime::RuntimeRunView, AppError> {
    let spec: RuntimeResumeRequest = required_argument(spec, "spec")?;
    spec.validate()?;
    let manager = runtime.runtime()?;
    tauri::async_runtime::spawn_blocking(move || manager.resume(&spec))
        .await
        .map_err(blocking_error)?
}

#[tauri::command]
async fn multicli_runtime_send(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::runtime::RuntimeRunView, AppError> {
    let query: RuntimeSendRequest = required_argument(query, "query")?;
    query.validate()?;
    let manager = runtime.runtime()?;
    tauri::async_runtime::spawn_blocking(move || manager.send(&query))
        .await
        .map_err(blocking_error)?
}

#[tauri::command]
async fn multicli_runtime_cancel(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::runtime::RuntimeRunView, AppError> {
    let query: RuntimeCancelRequest = required_argument(query, "query")?;
    query.validate()?;
    let manager = runtime.runtime()?;
    tauri::async_runtime::spawn_blocking(move || manager.cancel(&query))
        .await
        .map_err(blocking_error)?
}

#[tauri::command]
async fn multicli_runtime_answer(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::runtime::RuntimeRunView, AppError> {
    let query: RuntimeAnswerRequest = required_argument(query, "query")?;
    query.validate()?;
    let manager = runtime.runtime()?;
    tauri::async_runtime::spawn_blocking(move || manager.answer(&query))
        .await
        .map_err(blocking_error)?
}

#[tauri::command]
async fn multicli_runtime_stop(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::runtime::RuntimeRunView, AppError> {
    let query: RuntimeStopRequest = required_argument(query, "query")?;
    query.validate()?;
    let manager = runtime.runtime()?;
    tauri::async_runtime::spawn_blocking(move || manager.stop(&query))
        .await
        .map_err(blocking_error)?
}

#[tauri::command]
fn multicli_runtime_get(
    run_id: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::runtime::RuntimeRunView, AppError> {
    let run_id: String = required_argument(run_id, "runId")?;
    runtime.runtime()?.get(&run_id)
}

#[tauri::command]
fn multicli_runtime_read(
    query: Option<serde_json::Value>,
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::runtime::RuntimeEventPage, AppError> {
    let query: RuntimeReadRequest = required_argument(query, "query")?;
    query.validate()?;
    runtime.runtime()?.read(&query)
}

#[tauri::command]
fn multicli_runtime_list(
    runtime: State<'_, MultiCliRuntime>,
) -> Result<crate::multicli::runtime::RuntimeListView, AppError> {
    Ok(runtime.runtime()?.list())
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

fn show_main_window(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// The tray "退出" entry never exits by itself: it asks the window, which shows
/// the cancel-or-stop choice. A tray item that silently killed terminals would
/// violate the S04 exit rule.
fn request_quit(app: &tauri::AppHandle) {
    use tauri::Emitter;
    show_main_window(app);
    let _ = app.emit(QUIT_REQUESTED_EVENT, ());
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 MultiCLI", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut builder = TrayIconBuilder::with_id("multicli-tray")
        .tooltip("MultiCLI")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => request_quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
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
            let window = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("MultiCLI")
            .inner_size(1320.0, 820.0)
            .min_inner_size(960.0, 640.0)
            .data_directory(webview_directory)
            .build()?;
            {
                use tauri::Manager;
                let runtime = app.state::<MultiCliRuntime>();
                runtime.attach_app(app.handle().clone());
            }
            // Closing the main window hides it to the tray: managed terminals
            // keep draining in the background. Ending the application is an
            // explicit choice from the tray, never a side effect of a close.
            //
            // Hiding is only installed when the tray really exists: an app that
            // hides itself with no way back would be worse than an app that
            // exits on close, so a tray failure degrades to "close == exit"
            // (and `RunEvent::Exit` then ends only the owned trees).
            let tray_ready = match build_tray(app.handle()) {
                Ok(()) => true,
                Err(error) => {
                    eprintln!("[multicli] tray icon unavailable, close will exit: {error}");
                    false
                }
            };
            if tray_ready {
                let window_handle = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window_handle.hide();
                    }
                });
            }
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
            multicli_export_diagnostics,
            multicli_list_installations,
            multicli_list_processes,
            multicli_refresh_discovery,
            multicli_update_discovery_settings,
            multicli_list_history_sources,
            multicli_list_history_candidates,
            multicli_update_history_source,
            multicli_refresh_history,
            multicli_get_history_status,
            multicli_rebuild_history_index,
            multicli_get_session_metadata,
            multicli_list_session_messages,
            multicli_search_history,
            multicli_resolve_resume_target,
            multicli_terminal_targets,
            multicli_terminal_open,
            multicli_terminal_list,
            multicli_terminal_read,
            multicli_terminal_write,
            multicli_terminal_resize,
            multicli_terminal_stop,
            multicli_terminal_close,
            multicli_terminal_shutdown_all,
            multicli_terminal_limits,
            multicli_terminal_update_limits,
            multicli_terminal_quit_check,
            multicli_usage_overview,
            multicli_usage_refresh,
            multicli_profile_data,
            multicli_profile_backup,
            multicli_profile_restore,
            multicli_profile_cancel_restore,
            multicli_custom_cli_list,
            multicli_custom_cli_create,
            multicli_custom_cli_delete,
            multicli_runtime_capabilities,
            multicli_runtime_start,
            multicli_runtime_resume,
            multicli_runtime_send,
            multicli_runtime_cancel,
            multicli_runtime_answer,
            multicli_runtime_stop,
            multicli_runtime_get,
            multicli_runtime_read,
            multicli_runtime_list,
            multicli_app_quit
        ])
        .build(tauri::generate_context!())
        .expect("build MultiCLI")
        .run(|handle, event| {
            if let tauri::RunEvent::Exit = event {
                // Ends only this application's own sampling threads and its own
                // terminal trees. External CLIs keep running untouched.
                if let Some(runtime) = tauri::Manager::try_state::<MultiCliRuntime>(handle) {
                    runtime.stop_discovery();
                    runtime.stop_history();
                    runtime.stop_runtime();
                    runtime.stop_terminals();
                }
            }
        });
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
    fn usage_overview_and_a_filtered_refresh_pass_through_ipc() {
        let root = temp_root("ipc-usage");
        let app = tauri::test::mock_builder()
            .manage(MultiCliRuntime::new(root.clone(), "test".into()))
            .invoke_handler(tauri::generate_handler![
                multicli_usage_overview,
                multicli_usage_refresh,
                multicli_custom_cli_list
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let overview = tauri::test::get_ipc_response(
            &window,
            tauri::webview::InvokeRequest {
                cmd: "multicli_usage_overview".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "http://tauri.localhost".parse().unwrap(),
                body: tauri::ipc::InvokeBody::Json(json!({})),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .expect("usage overview");
        let value: serde_json::Value = overview.deserialize().unwrap();
        assert_eq!(value["schemaVersion"], 1);
        assert!(value["accounts"].as_array().unwrap().is_empty());
        assert!(value["unconfirmedPools"].as_array().unwrap().is_empty());
        // The catalogue always lists every family, so the UI never has to guess
        // which products exist.
        assert_eq!(value["catalog"].as_array().unwrap().len(), 10);

        // An unknown agent filter matches no provider: no CLI is started and
        // the call still returns a well-formed result instead of an error.
        let refreshed = tauri::test::get_ipc_response(
            &window,
            tauri::webview::InvokeRequest {
                cmd: "multicli_usage_refresh".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "http://tauri.localhost".parse().unwrap(),
                body: tauri::ipc::InvokeBody::Json(json!({"query":{"agentId":"not-a-product"}})),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .expect("usage refresh");
        let value: serde_json::Value = refreshed.deserialize().unwrap();
        assert!(value["outcomes"].as_array().unwrap().is_empty());
        assert_eq!(value["overview"]["schemaVersion"], 1);

        let custom = tauri::test::get_ipc_response(
            &window,
            tauri::webview::InvokeRequest {
                cmd: "multicli_custom_cli_list".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "http://tauri.localhost".parse().unwrap(),
                body: tauri::ipc::InvokeBody::Json(json!({})),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .expect("custom cli list");
        let value: serde_json::Value = custom.deserialize().unwrap();
        assert!(value.as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn profile_data_backup_and_planned_restore_pass_through_ipc() {
        let root = temp_root("ipc-profile-data");
        let app = tauri::test::mock_builder()
            .manage(MultiCliRuntime::new(root.clone(), "test".into()))
            .invoke_handler(tauri::generate_handler![
                multicli_profile_data,
                multicli_profile_backup,
                multicli_profile_restore,
                multicli_profile_cancel_restore
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let call = |cmd: &str, body: serde_json::Value| -> serde_json::Value {
            tauri::test::get_ipc_response(
                &window,
                tauri::webview::InvokeRequest {
                    cmd: cmd.into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: "http://tauri.localhost".parse().unwrap(),
                    body: tauri::ipc::InvokeBody::Json(body),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.to_string(),
                },
            )
            .unwrap_or_else(|error| panic!("{cmd}: {error:?}"))
            .deserialize()
            .unwrap()
        };

        let data = call("multicli_profile_data", json!({}));
        assert_eq!(data["profile"], "test");
        assert_eq!(data["expectedSchema"], 7);
        assert!(data["backups"].as_array().unwrap().is_empty());
        assert!(data["pendingRestore"].is_null());

        let backup = call("multicli_profile_backup", json!({}));
        assert_eq!(backup["integrity"], "ok");
        assert!(backup["manifest"]["sha256"].as_str().unwrap().len() == 64);
        let file = backup["manifest"]["fileName"]
            .as_str()
            .expect("file name")
            .to_string();
        assert!(file.ends_with(".db"));

        let listed = call("multicli_profile_data", json!({}));
        assert_eq!(listed["backups"].as_array().unwrap().len(), 1);

        // Planning a restore writes only the marker; the database is replaced
        // on the next start, never from this command.
        let planned = call(
            "multicli_profile_restore",
            json!({ "query": { "fileName": file } }),
        );
        assert_eq!(planned["backupFileName"], file);
        let pending = call("multicli_profile_data", json!({}));
        assert_eq!(pending["pendingRestore"]["backupFileName"], file);

        call("multicli_profile_cancel_restore", json!({}));
        let cancelled = call("multicli_profile_data", json!({}));
        assert!(cancelled["pendingRestore"].is_null());
        let _ = std::fs::remove_dir_all(root);
    }

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
                multicli_get_session,
                multicli_list_installations,
                multicli_list_processes,
                multicli_update_discovery_settings,
                multicli_usage_overview,
                multicli_usage_refresh,
                multicli_custom_cli_create
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
            ("multicli_list_installations", json!({})),
            ("multicli_list_installations", json!({"query":{"limit":0}})),
            (
                "multicli_list_installations",
                json!({"query":{"limit":20,"agentId":" "}}),
            ),
            (
                "multicli_list_processes",
                json!({"query":{"limit":20,"state":"not-a-state"}}),
            ),
            (
                "multicli_list_processes",
                json!({"query":{"limit":20,"workspaceId":"ws-1","unknown":1}}),
            ),
            (
                "multicli_update_discovery_settings",
                json!({"query":{"intervalMs":10}}),
            ),
            (
                "multicli_usage_refresh",
                json!({"query":{"unexpected":true}}),
            ),
            (
                "multicli_custom_cli_create",
                json!({"query":{"label":"x"}}),
            ),
            (
                "multicli_custom_cli_create",
                json!({"query":{"label":"x","executable":"relative.exe","args":[]}}),
            ),
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
        // CI's Windows temp directory may be a short-name/verbatim alias.
        // Compare filesystem identities while retaining the containment check.
        assert!(std::fs::canonicalize(&path)
            .unwrap()
            .starts_with(std::fs::canonicalize(root.join("diagnostics")).unwrap()));
        let text = std::fs::read_to_string(&path).unwrap();
        let document: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(document["schemaVersion"], 1);
        assert_eq!(document["profile"], "test");
        assert_eq!(document["counts"]["agents"], 3);
        assert_eq!(document["counts"]["workspaces"], 2);
        assert_eq!(document["counts"]["sessions"], 1);
        assert_eq!(document["counts"]["messages"], 1);
        assert_eq!(document["counts"]["runs"], 1);
        // The diagnostics file reports the *real* schema version; pinning a
        // literal here broke on every migration, so compare with the constant.
        assert_eq!(
            document["database"]["schemaVersion"],
            crate::multicli::SCHEMA_VERSION
        );
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

    #[test]
    fn a_failed_store_retry_waits_until_restart_to_apply_a_new_restore() {
        let root = temp_root("restore-retry-lifecycle");
        let initial = MultiCliService::open(&root).unwrap();
        initial.register_host(&Host::local("preserved-in-backup")).unwrap();
        let record = crate::multicli::backup::create_backup(
            &initial.store().connection().lock(), &root, "test", "0.1.0", "manual",
        ).unwrap();
        drop(initial);
        let corrupt = b"synthetic damaged database";
        std::fs::write(root.join("multicli.db"), corrupt).unwrap();
        let runtime = MultiCliRuntime::new(root.clone(), "test".into());
        assert!(runtime.service().is_err());
        runtime.plan_profile_restore(&record.manifest.file_name).unwrap();
        let data = runtime.profile_data().unwrap();
        assert!(data.pending_restore.is_some(), "refresh cannot apply a newly planned restore");
        assert!(data.last_restore.is_none());
        assert_eq!(std::fs::read(root.join("multicli.db")).unwrap(), corrupt);
        drop(runtime);
        let restarted = MultiCliRuntime::new(root.clone(), "test".into());
        assert!(restarted.service().is_ok());
        let data = restarted.profile_data().unwrap();
        assert!(data.pending_restore.is_none());
        assert!(data.last_restore.is_some());
    }

    #[test]
    fn a_runtime_takes_only_one_restore_attempt() {
        let runtime = MultiCliRuntime::new(
            std::env::temp_dir().join("multicli-restore-gate"),
            "test".to_string(),
        );
        assert!(runtime.take_restore_attempt());
        assert!(!runtime.take_restore_attempt());
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
