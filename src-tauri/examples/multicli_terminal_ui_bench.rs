//! Independent real-WebView S04 benchmark. Run beside the Vite server described
//! in tests/multicli-terminal-ui/README.md; all benchmark state lives in the
//! explicitly supplied scratch root.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ccgui_next_lib::multicli::terminal::{
    NotifyFn, TerminalCloseRequest, TerminalManager, TerminalNotify, TerminalOpenSpec,
    TerminalReadRequest, TerminalResizeRequest, TerminalStopMode, TerminalTarget,
    TerminalTargetKind, TerminalWriteRequest,
};
use ccgui_next_lib::multicli::{
    AgentDefinition, AgentInstallation, Host, HostPath, MultiCliService,
};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{Emitter, State};

const HOST_ID: &str = "local-windows";
const AGENT_ID: &str = "synthetic-ui-bench";
const INSTALLATION_ID: &str = "synthetic-ui-bench-flood-echo";
const TERMINAL_COUNT: usize = 8;
const EVENT_NAME: &str = "multicli://terminal";

struct Options {
    root: PathBuf,
    helper: PathBuf,
    dev_url: tauri::Url,
    rounds: u32,
    seconds: u32,
    samples: u32,
}

fn options() -> Result<Options, String> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .ok_or_else(|| "cannot resolve repository root".to_string())?
        .to_path_buf();
    let default_root = repo.join("scratch/S04/acceptance/ui-bench-runtime");
    let mut root = default_root.clone();
    let mut helper = default_root.join("flood-echo.exe");
    let mut dev_url = tauri::Url::parse("http://127.0.0.1:1437/")
        .map_err(|error| error.to_string())?;
    let mut rounds = 20;
    let mut seconds = 60;
    let mut samples = 12;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or_else(|| format!("missing value for {arg}"))?;
        match arg.as_str() {
            "--root" => root = PathBuf::from(value),
            "--helper" => helper = PathBuf::from(value),
            "--dev-url" => {
                dev_url = tauri::Url::parse(&value).map_err(|error| error.to_string())?
            }
            "--rounds" => rounds = value.parse().map_err(|error| format!("{error}"))?,
            "--seconds" => seconds = value.parse().map_err(|error| format!("{error}"))?,
            "--samples" => samples = value.parse().map_err(|error| format!("{error}"))?,
            other => return Err(format!("unknown option {other}")),
        }
    }
    if !root.is_absolute() || !helper.is_absolute() {
        return Err("--root and --helper must be absolute paths".into());
    }
    if !helper.is_file() {
        return Err(format!("flood helper does not exist: {}", helper.display()));
    }
    if dev_url.scheme() != "http"
        || dev_url.host_str() != Some("127.0.0.1")
        || dev_url.port_or_known_default() != Some(1437)
    {
        return Err("--dev-url must be the http://127.0.0.1:1437 loopback origin".into());
    }
    if !(1..=20).contains(&rounds) {
        return Err("--rounds must be between 1 and 20".into());
    }
    if !((1..=20).contains(&seconds) || seconds == 60) {
        return Err("--seconds must be 1..20 for smoke or exactly 60 for the full run".into());
    }
    if !(4..=40).contains(&samples) {
        return Err("--samples must be between 4 and 40".into());
    }
    Ok(Options { root, helper, dev_url, rounds, seconds, samples })
}

struct BenchState {
    root: PathBuf,
    manager: Arc<TerminalManager>,
    current_ids: Mutex<Vec<BenchTerminal>>,
    rounds: Mutex<Vec<Value>>,
    process_start_handles: Option<u32>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchStart {
    round: u32,
    terminals: Vec<BenchTerminal>,
    handles_before: Option<u32>,
    active_terminals_before: u32,
    active_terminals_after_open: u32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchTerminal {
    terminal_id: String,
    title: String,
    pid: Option<u32>,
    creation_time: Option<String>,
    opened_at_epoch_ms: u128,
}

#[tauri::command]
async fn bench_start(
    round: u32,
    seconds: u32,
    rate_kib: u32,
    state: State<'_, BenchState>,
) -> Result<BenchStart, String> {
    if !(1..=20).contains(&seconds) && seconds != 60 {
        return Err("seconds must be 1..20 for smoke or exactly 60 for the full run".into());
    }
    if !(1..=1024).contains(&rate_kib) {
        return Err("rateKib must be between 1 and 1024".into());
    }
    if rate_kib != 100 {
        return Err("this benchmark fixes each flooder at 100 KiB/s".into());
    }
    if !state.current_ids.lock().map_err(|_| "round lock poisoned")?.is_empty() {
        return Err("the previous round is still active".into());
    }
    if round == 0 {
        state.rounds.lock().map_err(|_| "report lock poisoned")?.clear();
    }

    let manager = Arc::clone(&state.manager);
    let cwd = state.root.join("work");
    let opened = tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&cwd).map_err(|error| error.to_string())?;
        let mut terminals: Vec<BenchTerminal> = Vec::with_capacity(TERMINAL_COUNT);
        for index in 0..TERMINAL_COUNT {
            let title = format!("ui-bench-{round}-{index}");
            let spec = TerminalOpenSpec {
                target: TerminalTarget {
                    kind: TerminalTargetKind::Installation,
                    shell: None,
                    installation_id: Some(INSTALLATION_ID.into()),
                },
            args: vec![
                    "--rate-kib".into(),
                    rate_kib.to_string(),
                    "--seconds".into(),
                    seconds.to_string(),
                ],
                cwd: cwd.display().to_string(),
                env: Vec::new(),
                cols: 100,
                rows: 28,
                title: Some(title.clone()),
            };
            let view = match manager.open(&spec) {
                Ok(view) => view,
                Err(error) => {
                    for terminal in &terminals {
                        let _ = manager.close(&TerminalCloseRequest {
                            terminal_id: terminal.terminal_id.clone(),
                            mode: Some(TerminalStopMode::Force),
                            grace_ms: Some(500),
                        });
                    }
                    return Err(error.message);
                }
            };
            terminals.push(BenchTerminal {
                terminal_id: view.terminal_id,
                title,
                pid: view.identity.as_ref().map(|identity| identity.pid),
                creation_time: view.identity.map(|identity| identity.creation_time),
                opened_at_epoch_ms: epoch_ms(),
            });
        }
        Ok::<_, String>(terminals)
    })
    .await
    .map_err(|error| format!("open terminal workers: {error}"))??;

    *state.current_ids.lock().map_err(|_| "round lock poisoned")? = opened.clone();
    let active = state.manager.list().active_count;
    Ok(BenchStart {
        round,
        terminals: opened,
        handles_before: handle_count(),
        active_terminals_before: active.saturating_sub(TERMINAL_COUNT as u32),
        active_terminals_after_open: active,
    })
}

#[tauri::command]
async fn bench_close(
    mut report: Value,
    state: State<'_, BenchState>,
) -> Result<Value, String> {
    let terminals = std::mem::take(&mut *state.current_ids.lock().map_err(|_| "round lock poisoned")?);
    let manager = Arc::clone(&state.manager);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let mut close_reports = Vec::with_capacity(terminals.len());
        let mut process_identities = Vec::with_capacity(terminals.len());
        for terminal in terminals {
            let close = manager
                .close(&TerminalCloseRequest {
                    terminal_id: terminal.terminal_id.clone(),
                    mode: Some(TerminalStopMode::Force),
                    grace_ms: Some(2_000),
                })
                .map_err(|error| error.message)?;
            process_identities.push((terminal.pid, terminal.creation_time));
            close_reports.push(json!({
                "removed": close.removed,
                "alreadyTerminal": close.stop.already_terminal,
                "escalated": close.stop.escalated,
                "waitedMs": close.stop.waited_ms,
            }));
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
        let process_checks = process_identities
            .iter()
            .map(|(pid, creation_time)| {
                let status = match (*pid, creation_time.as_deref()) {
                    (Some(pid), Some(created)) => process_identity_check(pid, created),
                    _ => "unverified_identity_missing".into(),
                };
                json!({ "pid": pid, "creationTime": creation_time, "status": status })
            })
            .collect::<Vec<_>>();
        Ok::<_, String>((close_reports, manager.list().active_count, process_checks))
    })
    .await
    .map_err(|error| format!("close terminal workers: {error}"))??;
    if let Some(object) = report.as_object_mut() {
        object.insert("closeReports".into(), result.0.into());
        object.insert("activeTerminalsAfterClose".into(), result.1.into());
        let checks = result.2;
        let survivors = checks
            .iter()
            .filter(|check| check.get("status").and_then(Value::as_str) == Some("same_identity_alive"))
            .cloned()
            .collect::<Vec<_>>();
        object.insert("syntheticProcessChecks".into(), checks.into());
        object.insert("syntheticProcessSurvivors".into(), survivors.into());
        object.insert("handlesAfter".into(), json!(handle_count()));
        object.insert("processTreeResourcesAfter".into(), process_tree_resources());
        let db = rusqlite::Connection::open_with_flags(state.root.join("profile/multicli.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e|e.to_string())?;
        let unfinished: i64 = db.query_row("SELECT COUNT(*) FROM runs WHERE ownership='managed' AND transport='pty' AND ended_at IS NULL", [], |row|row.get(0)).map_err(|e|e.to_string())?;
        object.insert("unfinalizedRunsAfterClose".into(), json!(unfinished));
    }
    Ok(report)
}

#[tauri::command]
fn bench_report(report: Value, state: State<'_, BenchState>) -> Result<Value, String> {
    let mut rounds = state.rounds.lock().map_err(|_| "report lock poisoned")?;
    rounds.push(report);
    let document = json!({
        "schemaVersion": 1,
        "generatedAt": epoch_ms(),
        "measurementScope": "Scheduled browser input deadline -> JS dispatch -> Windows WebView2 Tauri IPC -> ConPTY child stdin -> synthetic helper -> ConPTY output -> production TerminalManager read DTO -> production TerminalPane -> @xterm/xterm write callback -> two requestAnimationFrame callbacks",
        "limits": {
            "terminalsPerRound": TERMINAL_COUNT,
            "floodKibPerSecondEach": 100,
            "rounds": "frontend configurable; default 20, same Tauri process",
            "secondsPerRound": "frontend configurable; default 60",
            "samplesPerRound": "frontend configurable; 4..40"
        },
        "inputTimingNote": "Timing starts at a monotonic scheduled input deadline aligned with the common flood barrier, includes delayed JS dispatch, and ends after the matching helper ACK was parsed by production xterm write and two animation frames. invokeLatencyMs is also retained separately. It excludes OS keyboard injection and physical display scanout.",
        "root": state.root,
        "processStartHandles": state.process_start_handles,
        "rounds": rounds.clone(),
        "failures": rounds.iter().flat_map(|round| round.get("failures").and_then(Value::as_array).cloned().unwrap_or_default()).collect::<Vec<_>>(),
    });
    let path = state.root.join("bench-report.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())?)
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(json!({ "path": path, "rounds": rounds.len() }))
}

#[tauri::command]
fn bench_finish(app: tauri::AppHandle) { app.exit(0); }

#[tauri::command]
fn multicli_terminal_read(
    query: TerminalReadRequest,
    state: State<'_, BenchState>,
) -> Result<ccgui_next_lib::multicli::terminal::TerminalReadView, String> {
    state.manager.read(&query).map_err(|error| error.message)
}

#[tauri::command]
async fn multicli_terminal_write(
    query: TerminalWriteRequest,
    state: State<'_, BenchState>,
) -> Result<ccgui_next_lib::multicli::terminal::TerminalWriteView, String> {
    let manager = Arc::clone(&state.manager);
    tauri::async_runtime::spawn_blocking(move || manager.write(&query))
        .await
        .map_err(|error| format!("write task failed: {error}"))?
        .map_err(|error| error.message)
}

#[tauri::command]
fn multicli_terminal_resize(
    query: TerminalResizeRequest,
    state: State<'_, BenchState>,
) -> Result<ccgui_next_lib::multicli::terminal::TerminalResizeView, String> {
    state.manager.resize(&query).map_err(|error| error.message)
}

fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default()
}

#[cfg(windows)]
fn handle_count() -> Option<u32> {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    let mut count = 0;
    unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) }
        .is_ok()
        .then_some(count)
}

fn process_tree_resources() -> Value {
    use sysinfo::{System, ProcessesToUpdate, ProcessRefreshKind};
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_memory());
    let root = std::process::id();
    let Some(root_created) = process_created(root) else {
        return json!({"totalHandles":null,"unverifiedHandleCounts":1,"workingSetBytes":null,"processes":[],"error":"cannot verify root creation time"});
    };
    let mut ids = std::collections::HashMap::from([(root, root_created)]);
    let mut rejected_old_children = std::collections::HashSet::new();
    let mut unverified_parent_links = std::collections::HashSet::new();
    loop {
        let before = ids.len();
        for (pid, process) in system.processes() {
            if ids.contains_key(&pid.as_u32()) { continue; }
            if let Some(parent_created) = process.parent().and_then(|parent| ids.get(&parent.as_u32())) {
                match process_created(pid.as_u32()) {
                    Some(created) if created >= *parent_created => { ids.insert(pid.as_u32(), created); }
                    Some(_) => { rejected_old_children.insert(pid.as_u32()); }
                    None => { unverified_parent_links.insert(pid.as_u32()); }
                }
            }
        }
        if ids.len() == before { break; }
    }
    let mut processes = ids.into_iter().map(|(pid, created)| {
        let process = system.process(sysinfo::Pid::from_u32(pid));
        json!({"pid":pid,"name":process.map(|p| p.name().to_string_lossy().into_owned()),
            "creationTime100ns":created,
            "handles":process_handles(pid),"workingSetBytes":process.map(|p|p.memory()),
            "virtualMemoryBytes":process.map(|p|p.virtual_memory())})
    }).collect::<Vec<_>>();
    processes.sort_by_key(|p|p["pid"].as_u64());
    json!({"totalHandles":processes.iter().filter_map(|p|p["handles"].as_u64()).sum::<u64>(),
        "unverifiedHandleCounts":processes.iter().filter(|p|p["handles"].is_null()).count(),
        "workingSetBytes":processes.iter().filter_map(|p|p["workingSetBytes"].as_u64()).sum::<u64>(),
        "rejectedReusedParentLinks":rejected_old_children.len(),
        "unverifiedParentLinks":unverified_parent_links.len(),
        "processes":processes})
}

#[cfg(windows)]
fn process_created(pid: u32) -> Option<u64> {
    use windows::Win32::{Foundation::{CloseHandle, FILETIME}, System::Threading::{OpenProcess, GetProcessTimes, PROCESS_QUERY_LIMITED_INFORMATION}};
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let result = GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user).ok()
            .map(|_| ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64);
        let _ = CloseHandle(process);
        result
    }
}

#[cfg(not(windows))]
fn process_created(_pid: u32) -> Option<u64> { None }

#[cfg(windows)]
fn process_handles(pid: u32) -> Option<u32> {
    use windows::Win32::{Foundation::CloseHandle, System::Threading::{OpenProcess, GetProcessHandleCount, PROCESS_QUERY_LIMITED_INFORMATION}};
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut count = 0;
        let result = GetProcessHandleCount(process, &mut count).ok().map(|_|count);
        let _ = CloseHandle(process);
        result
    }
}

#[cfg(not(windows))]
fn process_handles(_pid: u32) -> Option<u32> { None }

#[cfg(windows)]
fn process_identity_check(pid: u32, expected_creation_time: &str) -> String {
    // A terminated process can remain an OS object while handles are being
    // released, and its image path is no longer queryable. Check the recorded
    // identity and real exit code without requiring a readable image path.
    use windows::Win32::{Foundation::{CloseHandle, FILETIME}, System::Threading::{OpenProcess, GetProcessTimes, GetExitCodeProcess, PROCESS_QUERY_LIMITED_INFORMATION}};
    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
            let mut code = 259;
            let times = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user).is_ok();
            let exit_read = GetExitCodeProcess(handle, &mut code).is_ok();
            let _ = CloseHandle(handle);
            if times && exit_read {
                let ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
                if ccgui_next_lib::multicli::discovery::winproc::filetime_to_iso(ticks).as_deref() != Some(expected_creation_time) {
                    return "original_gone_pid_reused".into();
                }
                if code != 259 { return format!("exited_with_code_{code}"); }
            }
        }
    }
    use ccgui_next_lib::multicli::discovery::winproc::{
        BackendFailure, HostBackend, WindowsProcessBackend,
    };
    match WindowsProcessBackend::new(HOST_ID).inspect(pid) {
        Ok(process) if process.creation_time == expected_creation_time => "same_identity_alive".into(),
        Ok(_) => "original_gone_pid_reused".into(),
        Err(error) if error.kind == BackendFailure::Gone => "gone".into(),
        Err(error) => format!("unverified: {:?}: {}", error.kind, error.reason),
    }
}

#[cfg(not(windows))]
fn process_identity_check(_pid: u32, _expected_creation_time: &str) -> String {
    "unverified_non_windows".into()
}

#[cfg(not(windows))]
fn handle_count() -> Option<u32> {
    None
}

fn main() {
    if let Err(error) = run() {
        eprintln!("multicli_terminal_ui_bench: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = options()?;
    let profile = options.root.join("profile");
    let webview_data = options.root.join("webview");
    std::fs::create_dir_all(&profile).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&webview_data).map_err(|error| error.to_string())?;

    let service = Arc::new(MultiCliService::open(&profile).map_err(|error| error.message)?);
    service
        .register_host(&Host::local(HOST_ID))
        .map_err(|error| error.message)?;
    service
        .register_agent(&AgentDefinition {
            id: AGENT_ID.into(),
            vendor: "synthetic-benchmark".into(),
            display_name: "Synthetic Terminal UI Benchmark".into(),
        })
        .map_err(|error| error.message)?;
    service
        .register_installation(&AgentInstallation {
            id: INSTALLATION_ID.into(),
            host_id: HOST_ID.into(),
            agent_id: AGENT_ID.into(),
            executable: HostPath {
                host_id: HOST_ID.into(),
                display_path: options.helper.display().to_string(),
                canonical_path: Some(options.helper.canonicalize().map_err(|error| error.to_string())?.display().to_string()),
            },
            version: Some("synthetic-ui-bench".into()),
            config_profile_id: "scratch-ui-bench".into(),
        })
        .map_err(|error| error.message)?;

    let app_handle_for_notify = Arc::new(Mutex::new(None::<tauri::AppHandle>));
    let notify_handle = Arc::clone(&app_handle_for_notify);
    let notifier: NotifyFn = Arc::new(move |event: TerminalNotify| {
        if let Ok(handle) = notify_handle.lock() {
            if let Some(handle) = handle.as_ref() {
                let _ = handle.emit(EVENT_NAME, event);
            }
        }
    });
    let manager = Arc::new(
        TerminalManager::new(
            Arc::clone(&service),
            profile,
            "ui-bench".into(),
            notifier,
        )
        .map_err(|error| error.message)?,
    );
    let state = BenchState {
        root: options.root.clone(),
        manager: Arc::clone(&manager),
        current_ids: Mutex::new(Vec::new()),
        rounds: Mutex::new(Vec::new()),
        process_start_handles: handle_count(),
    };

    let dev_url = options.dev_url;
    let frontend_path = format!(
        "ui-bench.html?autostart=1&rounds={}&seconds={}&samples={}",
        options.rounds, options.seconds, options.samples
    );
    let mut context = tauri::generate_context!();
    context.config_mut().build.dev_url = Some(dev_url);
    context.config_mut().app.windows.clear();
    context.config_mut().app.security.csp = Some(tauri::utils::config::Csp::Policy(
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ipc: http://ipc.localhost ws://127.0.0.1:1437 http://127.0.0.1:1437; font-src 'self' data:".into(),
    ));

    let builder = tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            bench_start,
            bench_close,
            bench_report,
            bench_finish,
            multicli_terminal_read,
            multicli_terminal_write,
            multicli_terminal_resize,
        ])
        .setup(move |app| {
            if let Ok(mut slot) = app_handle_for_notify.lock() {
                *slot = Some(app.handle().clone());
            }
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App(frontend_path.into()),
            )
            .title("MultiCLI S04 Terminal UI Benchmark")
            .inner_size(1440.0, 960.0)
            .data_directory(webview_data)
            .build()?;
            Ok(())
        });

    builder
        .build(context)
        .map_err(|error| format!("build benchmark Tauri app: {error}"))?
        .run(move |_handle, event| {
            if let tauri::RunEvent::Exit = event {
                manager.hard_shutdown_all();
            }
        });
    Ok(())
}
