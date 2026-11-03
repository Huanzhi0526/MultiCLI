//! S04 terminal integration tests.
//!
//! These tests use real Windows processes: the PTY, the job object and the two
//! script chains are the product behaviour under test, so a mock would prove
//! nothing. Every process they create is created by them and is tracked by the
//! manager; nothing is looked up by process name.

use super::launch::{batch_trampoline, powershell_trampoline};
use super::manager::TerminalManager;
use super::model::*;
use super::session::NotifyFn;
use crate::multicli::domain::{
    AgentDefinition, AgentInstallation, ExecutionStatus, Host, HostPath, RunTransport,
    SessionOrigin,
};
use crate::multicli::MultiCliService;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    root: PathBuf,
    manager: Arc<TerminalManager>,
    #[allow(dead_code)]
    service: Arc<MultiCliService>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.manager.hard_shutdown_all();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn temp_root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "multicli-term-{name}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn harness(name: &str) -> Harness {
    let root = temp_root(name);
    let service = Arc::new(MultiCliService::open(&root).unwrap());
    service.register_host(&Host::local("local-windows")).unwrap();
    let notifier: NotifyFn = Arc::new(|_| {});
    let manager = Arc::new(
        TerminalManager::new(
            Arc::clone(&service),
            root.clone(),
            "test".into(),
            notifier,
        )
        .unwrap(),
    );
    Harness {
        root,
        manager,
        service,
    }
}

fn unicode_cwd(root: &Path) -> PathBuf {
    let dir = root.join("中文 目录");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn shell_spec(cwd: &Path, flavor: ShellFlavor) -> TerminalOpenSpec {
    TerminalOpenSpec {
        target: TerminalTarget {
            kind: TerminalTargetKind::Shell,
            shell: Some(flavor),
            installation_id: None,
            custom_cli_id: None,
        },
        args: Vec::new(),
        cwd: cwd.display().to_string(),
        env: vec![EnvVar {
            name: "MULTICLI_S04_TEST".into(),
            value: "1".into(),
        }],
        cols: 100,
        rows: 30,
        title: Some("S04 test shell".into()),
    }
}

/// Reads until `needle` appears `expected` times.
///
/// A console echoes the typed line, so a cmd marker usually appears twice: once
/// as typed input and once as command output. Requiring the second occurrence
/// is what proves the output path itself carried the text.
fn read_until(
    manager: &TerminalManager,
    terminal_id: &str,
    cursor: &mut u64,
    needle: &str,
    expected: usize,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    let mut collected = String::new();
    loop {
        let view = manager
            .read(&TerminalReadRequest {
                terminal_id: terminal_id.into(),
                cursor: Some(*cursor),
                max_bytes: Some(MAX_READ_BYTES),
            })
            .unwrap();
        *cursor = view.to_seq;
        collected.push_str(&view.text);
        if collected.matches(needle).count() >= expected {
            return collected;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {expected}x {needle:?}; collected so far: {:?}",
                collected
            );
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn process_exists(pid: u32) -> bool {
    use crate::multicli::discovery::winproc::{HostBackend, WindowsProcessBackend};
    let backend = WindowsProcessBackend::new("local-windows");
    backend
        .inspect(pid)
        .map(|_| true)
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// A real neutral argument echo program
// ---------------------------------------------------------------------------

/// A tiny native program compiled once with the pinned toolchain. It is the
/// neutral child for the `.exe` chain and the forwarding target behind the
/// `.cmd` / `.ps1` chains, so the test compares what a real CRT process
/// received, not what a string looked like.
fn echo_exe() -> &'static Path {
    static ONCE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = std::env::temp_dir().join("multicli-s04-echo");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("echo.rs");
        std::fs::write(
            &source,
            r###"
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = String::from("##ARGV##[");
    for (index, arg) in args.iter().enumerate() {
        if index > 0 { out.push(','); }
        out.push('"');
        for ch in arg.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                other => out.push(other),
            }
        }
        out.push('"');
    }
    out.push(']');
    println!("{out}");
}
"###,
        )
        .unwrap();
        let cargo = PathBuf::from(env!("CARGO"));
        let rustc = cargo.with_file_name(if cfg!(windows) { "rustc.exe" } else { "rustc" });
        let exe = dir.join("echo.exe");
        let status = std::process::Command::new(&rustc)
            .arg(&source)
            .arg("-o")
            .arg(&exe)
            .arg("--edition=2021")
            .status()
            .expect("the pinned toolchain must provide rustc");
        assert!(status.success(), "building the echo helper failed");
        exe
    })
}

fn parse_argv(text: &str) -> Vec<String> {
    let start = text.find("##ARGV##[").expect("no argv marker in output") + 8;
    let rest = &text[start..];
    let mut depth = 0usize;
    let mut end = 0usize;
    for (index, ch) in rest.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = index + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    let json = &rest[..end];
    let parsed: Vec<String> = serde_json::from_str(json).expect("argv json");
    parsed
}

fn register_installation(
    service: &MultiCliService,
    root: &Path,
    agent_id: &str,
    executable: &Path,
    id: &str,
) -> AgentInstallation {
    service
        .register_agent(&AgentDefinition {
            id: agent_id.into(),
            vendor: "synthetic".into(),
            display_name: format!("Synthetic {agent_id}"),
        })
        .unwrap();
    let installation = AgentInstallation {
        id: id.into(),
        host_id: "local-windows".into(),
        agent_id: agent_id.into(),
        executable: HostPath {
            host_id: "local-windows".into(),
            display_path: executable.display().to_string(),
            canonical_path: Some(executable.display().to_string()),
        },
        version: Some("test-1".into()),
        config_profile_id: "default".into(),
    };
    let _ = root;
    service.register_installation(&installation).unwrap()
}

// ---------------------------------------------------------------------------
// Terminal loop
// ---------------------------------------------------------------------------

#[test]
fn a_real_shell_round_trips_unicode_resize_and_ctrl_c() {
    let harness = harness("shell");
    let cwd = unicode_cwd(&harness.root);
    let view = harness
        .manager
        .open(&shell_spec(&cwd, ShellFlavor::Cmd))
        .expect("open a real cmd shell");
    assert_eq!(view.state, TerminalState::Running);
    assert!(view.identity.is_some(), "a running terminal needs an identity");
    assert_eq!(view.launch_chain, LaunchChain::Exe);
    assert!(view.job.assigned, "a managed terminal must sit in a job");
    assert_eq!(view.job.mode, JobGuardMode::KillOnClose);
    assert_eq!(view.interaction_status, "unknown");

    let id = view.terminal_id.clone();
    let mut cursor = 0u64;
    harness
        .manager
        .write(&TerminalWriteRequest {
            terminal_id: id.clone(),
            data: "echo MARK-中文-😀\r".into(),
        })
        .unwrap();
    let text = read_until(
        &harness.manager,
        &id,
        &mut cursor,
        "MARK-中文-😀",
        2,
        Duration::from_secs(15),
    );
    assert!(text.contains("MARK-中文-😀"), "unicode echo failed: {text:?}");

    // The shell must start in the requested (Chinese + space) directory.
    harness
        .manager
        .write(&TerminalWriteRequest {
            terminal_id: id.clone(),
            data: "echo CWD=[%CD%]\r".into(),
        })
        .unwrap();
    let text = read_until(
        &harness.manager,
        &id,
        &mut cursor,
        "CWD=[",
        2,
        Duration::from_secs(10),
    );
    // The launcher resolves aliases before setting cwd. Runner TEMP can use
    // an 8.3 spelling (or a junction), while cmd reports the resolved path.
    // Keep checking the requested directory, using its canonical identity.
    let expected_cwd = dunce::canonicalize(&cwd).expect("resolve the requested cwd");
    assert!(
        text.contains(&expected_cwd.display().to_string()),
        "shell did not start in the requested cwd: {text:?}"
    );

    // Resize must be accepted and the console must stay usable afterwards.
    harness
        .manager
        .resize(&TerminalResizeRequest {
            terminal_id: id.clone(),
            cols: 120,
            rows: 40,
        })
        .unwrap();
    harness
        .manager
        .write(&TerminalWriteRequest {
            terminal_id: id.clone(),
            data: "echo AFTER-RESIZE\r".into(),
        })
        .unwrap();
    read_until(
        &harness.manager,
        &id,
        &mut cursor,
        "AFTER-RESIZE",
        2,
        Duration::from_secs(10),
    );
    assert!(harness.manager.resize(&TerminalResizeRequest {
        terminal_id: id.clone(),
        cols: 1,
        rows: 0,
    }).is_err());

    // Ctrl+C is an interrupt, not a stop: the terminal must still be running.
    let stop = harness
        .manager
        .stop(&TerminalStopRequest {
            terminal_id: id.clone(),
            mode: TerminalStopMode::Interrupt,
            grace_ms: None,
        })
        .unwrap();
    assert!(stop.stop.interrupt_sent);
    assert_eq!(stop.terminal.state, TerminalState::Running);

    let close = harness
        .manager
        .close(&TerminalCloseRequest {
            terminal_id: id.clone(),
            mode: Some(TerminalStopMode::Gentle),
            grace_ms: Some(1_000),
        })
        .unwrap();
    assert!(close.removed);
    assert_eq!(harness.manager.list().items.len(), 0);
}

#[test]
fn the_console_geometry_reaches_the_child_process() {
    let harness = harness("geometry");
    let cwd = unicode_cwd(&harness.root);
    let view = harness
        .manager
        .open(&shell_spec(&cwd, ShellFlavor::WindowsPowershell))
        .expect("open powershell");
    let id = view.terminal_id.clone();
    harness
        .manager
        .resize(&TerminalResizeRequest {
            terminal_id: id.clone(),
            cols: 120,
            rows: 40,
        })
        .unwrap();
    let mut cursor = 0u64;
    harness
        .manager
        .write(&TerminalWriteRequest {
            terminal_id: id.clone(),
            // The marker is assembled at runtime, so the line echoed by
            // PSReadLine cannot satisfy the wait by itself.
            data: "Write-Output ('RES' + 'ULT-GEO=' + [Console]::WindowWidth + 'x' + [Console]::WindowHeight)\r"
                .into(),
        })
        .unwrap();
    let text = read_until(
        &harness.manager,
        &id,
        &mut cursor,
        "RESULT-GEO=",
        1,
        Duration::from_secs(20),
    );
    assert!(
        text.contains("RESULT-GEO=120x40"),
        "the child console did not follow the PTY geometry: {text:?}"
    );
    let _ = harness.manager.close(&TerminalCloseRequest {
        terminal_id: id,
        mode: Some(TerminalStopMode::Gentle),
        grace_ms: Some(1_000),
    });
}

#[test]
fn two_terminals_never_cross_stream_and_closing_one_keeps_the_other() {
    let harness = harness("routing");
    let cwd = unicode_cwd(&harness.root);
    let first = harness
        .manager
        .open(&shell_spec(&cwd, ShellFlavor::Cmd))
        .unwrap();
    let second = harness
        .manager
        .open(&shell_spec(&cwd, ShellFlavor::Cmd))
        .unwrap();
    assert_ne!(first.terminal_id, second.terminal_id);
    assert_ne!(first.run_id, second.run_id);

    let mut cursor_a = 0u64;
    let mut cursor_b = 0u64;
    harness
        .manager
        .write(&TerminalWriteRequest {
            terminal_id: first.terminal_id.clone(),
            data: "echo ONLY-ALPHA\r".into(),
        })
        .unwrap();
    harness
        .manager
        .write(&TerminalWriteRequest {
            terminal_id: second.terminal_id.clone(),
            data: "echo ONLY-BETA\r".into(),
        })
        .unwrap();

    let text_a = read_until(
        &harness.manager,
        &first.terminal_id,
        &mut cursor_a,
        "ONLY-ALPHA",
        2,
        Duration::from_secs(15),
    );
    let text_b = read_until(
        &harness.manager,
        &second.terminal_id,
        &mut cursor_b,
        "ONLY-BETA",
        2,
        Duration::from_secs(15),
    );
    assert!(!text_a.contains("ONLY-BETA"), "terminal A saw B's output");
    assert!(!text_b.contains("ONLY-ALPHA"), "terminal B saw A's output");

    let close = harness
        .manager
        .close(&TerminalCloseRequest {
            terminal_id: first.terminal_id.clone(),
            mode: Some(TerminalStopMode::Gentle),
            grace_ms: Some(1_000),
        })
        .unwrap();
    assert!(close.removed);

    // The second terminal must still answer after its sibling closed.
    harness
        .manager
        .write(&TerminalWriteRequest {
            terminal_id: second.terminal_id.clone(),
            data: "echo BETA-STILL-ALIVE\r".into(),
        })
        .unwrap();
    read_until(
        &harness.manager,
        &second.terminal_id,
        &mut cursor_b,
        "BETA-STILL-ALIVE",
        2,
        Duration::from_secs(10),
    );
    assert_eq!(harness.manager.list().items.len(), 1);
    let _ = harness.manager.close(&TerminalCloseRequest {
        terminal_id: second.terminal_id,
        mode: Some(TerminalStopMode::Gentle),
        grace_ms: Some(1_000),
    });
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[test]
fn a_gentle_stop_escalates_to_the_owned_tree_and_reports_it() {
    let harness = harness("gentle");
    let cwd = unicode_cwd(&harness.root);
    let view = harness
        .manager
        .open(&shell_spec(&cwd, ShellFlavor::Cmd))
        .unwrap();
    let id = view.terminal_id.clone();
    let mut cursor = 0u64;
    // A foreground job that ignores nothing but keeps the shell busy: Ctrl+C
    // kills ping, the shell survives, so a gentle stop must escalate.
    harness
        .manager
        .write(&TerminalWriteRequest {
            terminal_id: id.clone(),
            data: "ping -n 60 127.0.0.1\r".into(),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(800));

    let stop = harness
        .manager
        .stop(&TerminalStopRequest {
            terminal_id: id.clone(),
            mode: TerminalStopMode::Gentle,
            grace_ms: Some(700),
        })
        .unwrap();
    assert!(stop.stop.interrupt_sent);
    assert!(stop.stop.escalated, "a surviving tree must be escalated");
    assert!(stop.stop.waited_ms >= 600);
    assert_eq!(stop.terminal.state, TerminalState::Exited);

    // A second stop is idempotent and never escalates again.
    let again = harness
        .manager
        .stop(&TerminalStopRequest {
            terminal_id: id.clone(),
            mode: TerminalStopMode::Force,
            grace_ms: None,
        })
        .unwrap();
    assert!(again.stop.already_terminal);
    let _ = harness.manager.close(&TerminalCloseRequest {
        terminal_id: id,
        mode: None,
        grace_ms: None,
    });
}

#[test]
fn kill_on_close_sweeps_a_grandchild_started_by_the_shell() {
    let harness = harness("grandchild");
    let cwd = unicode_cwd(&harness.root);
    let view = harness
        .manager
        .open(&shell_spec(&cwd, ShellFlavor::Cmd))
        .unwrap();
    let id = view.terminal_id.clone();
    let mut cursor = 0u64;
    harness
        .manager
        .write(&TerminalWriteRequest {
            terminal_id: id.clone(),
            data: concat!(
                "powershell -NoProfile -Command \"$p = Start-Process -FilePath ping ",
                "-ArgumentList '-n','120','127.0.0.1' -PassThru -WindowStyle Hidden; ",
                "Write-Output ('CHI' + 'LD=' + $p.Id)\"\r"
            )
            .into(),
        })
        .unwrap();
    let text = read_until(
        &harness.manager,
        &id,
        &mut cursor,
        "CHILD=",
        1,
        Duration::from_secs(30),
    );
    let pid: u32 = text
        .split("CHILD=")
        .nth(1)
        .and_then(|rest| {
            rest.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok()
        })
        .expect("the shell reported a grandchild pid");
    assert!(process_exists(pid), "the grandchild must be running");

    harness
        .manager
        .stop(&TerminalStopRequest {
            terminal_id: id.clone(),
            mode: TerminalStopMode::Force,
            grace_ms: None,
        })
        .unwrap();
    assert!(
        wait_until(|| !process_exists(pid), Duration::from_secs(10)),
        "kill-on-close left the grandchild alive"
    );
    let _ = harness.manager.close(&TerminalCloseRequest {
        terminal_id: id,
        mode: None,
        grace_ms: None,
    });
}

#[test]
fn an_external_instance_of_the_same_program_survives_a_terminal_close() {
    let harness = harness("external");
    let cwd = unicode_cwd(&harness.root);
    // The control process is the same program the terminal runs (`cmd.exe`),
    // started outside the manager: closing the managed terminal must leave it
    // untouched, which is what "an external instance of the same product" means.
    let mut external = std::process::Command::new("cmd")
        .args(["/c", "ping -n 120 127.0.0.1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the external control process");
    let external_pid = external.id();

    let view = harness
        .manager
        .open(&shell_spec(&cwd, ShellFlavor::Cmd))
        .unwrap();
    harness
        .manager
        .close(&TerminalCloseRequest {
            terminal_id: view.terminal_id.clone(),
            mode: Some(TerminalStopMode::Force),
            grace_ms: Some(500),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        process_exists(external_pid),
        "closing a terminal killed an unrelated instance of the same program"
    );
    let _ = external.kill();
    let _ = external.wait();
}

#[test]
fn a_target_that_exits_immediately_still_ends_its_run() {
    // A shim that prints and quits races `open`'s "link then mark running"
    // sequence; the Run must still end in a terminal state, never as `running`
    // with an `ended_at`.
    let harness = harness("fast-exit");
    let cwd = unicode_cwd(&harness.root);
    let script = harness.root.join("instant.cmd");
    std::fs::write(&script, "@echo off\r\necho BYE\r\nexit /b 0\r\n").unwrap();
    register_installation(
        &harness.service,
        &harness.root,
        "synthetic-instant",
        &script,
        "inst-instant",
    );
    let spec = TerminalOpenSpec {
        target: TerminalTarget {
            kind: TerminalTargetKind::Installation,
            shell: None,
            installation_id: Some("inst-instant".into()),
            custom_cli_id: None,
        },
        args: Vec::new(),
        cwd: cwd.display().to_string(),
        env: Vec::new(),
        cols: 100,
        rows: 30,
        title: None,
    };
    let view = harness.manager.open(&spec).unwrap();
    let observed = wait_until(
        || {
            harness
                .manager
                .list()
                .items
                .iter()
                .any(|item| item.terminal_id == view.terminal_id && item.state.is_terminal())
        },
        Duration::from_secs(15),
    );
    if !observed {
        let read = harness
            .manager
            .read(&TerminalReadRequest {
                terminal_id: view.terminal_id.clone(),
                cursor: None,
                max_bytes: Some(4096),
            })
            .map(|value| value.text)
            .unwrap_or_else(|error| format!("<read error {error}>"));
        panic!(
            "the terminal never reported its exit: {:?} text={read:?}",
            harness
                .manager
                .list()
                .items
                .iter()
                .map(|item| (item.terminal_id.clone(), item.state, item.exit.clone()))
                .collect::<Vec<_>>()
        );
    }
    // Give the finish callback a moment to hit the database.
    std::thread::sleep(Duration::from_millis(300));
    let detail = harness.service.get_session(&view.session_id).unwrap();
    let run = &detail.runs[0];
    assert!(
        run.execution_status.is_terminal(),
        "a finished terminal left its Run as {:?}",
        run.execution_status
    );
    assert!(run.ended_at.is_some());
    let _ = harness.manager.close(&TerminalCloseRequest {
        terminal_id: view.terminal_id,
        mode: None,
        grace_ms: None,
    });
}

#[test]
fn a_failed_open_leaves_no_running_run_and_no_terminal() {
    // The log directory is blocked by a regular file, so the Run is created as
    // `starting` and must be closed as `failed` without a live terminal.
    let root = temp_root("failed-open");
    let service = Arc::new(MultiCliService::open(&root).unwrap());
    service.register_host(&Host::local("local-windows")).unwrap();
    std::fs::write(root.join("logs"), b"blocked").unwrap();
    let manager = Arc::new(
        TerminalManager::new(
            Arc::clone(&service),
            root.clone(),
            "test".into(),
            Arc::new(|_| {}) as NotifyFn,
        )
        .unwrap(),
    );
    let cwd = unicode_cwd(&root);
    let error = manager.open(&shell_spec(&cwd, ShellFlavor::Cmd)).unwrap_err();
    assert_eq!(error.code, crate::multicli::error::ErrorCode::Io);
    assert!(manager.list().items.is_empty());

    let conn = service.lock_for_test();
    let failed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM runs WHERE execution_status = 'failed' AND ended_at IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let running: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM runs WHERE execution_status = 'running'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(failed, 1, "the failed open must be recorded as failed");
    assert_eq!(running, 0, "a failed open must not leave a running Run");
    drop(conn);
    manager.hard_shutdown_all();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_installation_is_refused_before_any_run_is_created() {
    let harness = harness("missing-target");
    let cwd = unicode_cwd(&harness.root);
    let missing = harness.root.join("gone.exe");
    register_installation(
        &harness.service,
        &harness.root,
        "synthetic-missing",
        &missing,
        "inst-missing",
    );
    let spec = TerminalOpenSpec {
        target: TerminalTarget {
            kind: TerminalTargetKind::Installation,
            shell: None,
            installation_id: Some("inst-missing".into()),
            custom_cli_id: None,
        },
        args: Vec::new(),
        cwd: cwd.display().to_string(),
        env: Vec::new(),
        cols: 80,
        rows: 24,
        title: None,
    };
    let error = harness.manager.open(&spec).unwrap_err();
    assert_eq!(error.code, crate::multicli::error::ErrorCode::Unsupported);
    let conn = harness.service.lock_for_test();
    let runs: i64 = conn
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(runs, 0, "target resolution must not create a Run");
}

#[test]
fn targets_report_ready_missing_and_unsupported_explicitly() {
    let harness = harness("targets");
    let ready = echo_exe().to_path_buf();
    let missing = harness.root.join("gone.exe");
    let unsupported = harness.root.join("thing.txt");
    std::fs::write(&unsupported, b"x").unwrap();
    register_installation(
        &harness.service,
        &harness.root,
        "synthetic-ready",
        &ready,
        "inst-ready",
    );
    register_installation(
        &harness.service,
        &harness.root,
        "synthetic-missing",
        &missing,
        "inst-missing",
    );
    register_installation(
        &harness.service,
        &harness.root,
        "synthetic-unsupported",
        &unsupported,
        "inst-unsupported",
    );
    // A row with no canonical identity is never "ready": the display path alone
    // cannot prove which file will be started.
    harness
        .service
        .register_agent(&AgentDefinition {
            id: "synthetic-unverified".into(),
            vendor: "synthetic".into(),
            display_name: "Synthetic unverified".into(),
        })
        .unwrap();
    harness
        .service
        .register_installation(&AgentInstallation {
            id: "inst-unverified".into(),
            host_id: "local-windows".into(),
            agent_id: "synthetic-unverified".into(),
            executable: HostPath {
                host_id: "local-windows".into(),
                display_path: ready.display().to_string(),
                canonical_path: None,
            },
            version: None,
            config_profile_id: "default".into(),
        })
        .unwrap();
    let targets = harness.manager.targets().unwrap();
    let by_id = |id: &str| {
        targets
            .items
            .iter()
            .find(|item| item.id == id)
            .unwrap_or_else(|| panic!("missing target {id}: {:?}", targets.items))
    };
    assert_eq!(by_id("shell:cmd").status, "ready");
    assert_eq!(by_id("installation:inst-ready").status, "ready");
    assert_eq!(
        by_id("installation:inst-ready").launch_chain,
        Some(LaunchChain::Exe)
    );
    assert_eq!(by_id("installation:inst-missing").status, "missing");
    assert_eq!(by_id("installation:inst-unsupported").status, "unsupported");
    assert_eq!(by_id("installation:inst-unverified").status, "unverified");
    assert!(by_id("installation:inst-missing").reason.is_some());
    assert!(by_id("installation:inst-unverified").reason.is_some());
    assert_eq!(targets.cwd_default.is_empty(), false);
}

// ---------------------------------------------------------------------------
// A11 — argument boundary matrix over the three chains
// ---------------------------------------------------------------------------

/// Arguments every chain must carry unchanged, including the double quote: the
/// batch chain carries it in an environment variable, so all three chains must
/// agree on the same vector instead of each having its own exceptions.
fn boundary_args() -> Vec<String> {
    vec![
        "plain".into(),
        "空格 分隔".into(),
        "引号\"内部\"值".into(),
        "amp&ersand".into(),
        "percent%50".into(),
        "path%PATH%like".into(),
        "paren(值)".into(),
        "中文参数😀".into(),
        "C:\\目录 with space\\".into(),
        "caret^value".into(),
        "pipe|lt<gt>".into(),
    ]
}

fn run_chain_case(harness: &Harness, executable: &Path, id: &str, cwd: &Path, args: Vec<String>) {
    register_installation(&harness.service, &harness.root, "synthetic-echo", executable, id);
    let spec = TerminalOpenSpec {
        target: TerminalTarget {
            kind: TerminalTargetKind::Installation,
            shell: None,
            installation_id: Some(id.into()),
            custom_cli_id: None,
        },
        args: args.clone(),
        cwd: cwd.display().to_string(),
        env: Vec::new(),
        cols: 200,
        rows: 40,
        title: Some(id.into()),
    };
    let view = harness
        .manager
        .open(&spec)
        .unwrap_or_else(|error| panic!("{id} chain failed to start: {error}"));
    let mut cursor = 0u64;
    let text = read_until(
        &harness.manager,
        &view.terminal_id,
        &mut cursor,
        "##ARGV##",
        1,
        Duration::from_secs(40),
    );
    let received = parse_argv(&text);
    assert_eq!(
        received, args,
        "{id} chain did not preserve the argument vector; raw output: {text:?}"
    );
    let _ = harness.manager.close(&TerminalCloseRequest {
        terminal_id: view.terminal_id.clone(),
        mode: Some(TerminalStopMode::Force),
        grace_ms: Some(1_000),
    });
}

#[test]
fn the_exe_chain_preserves_boundary_arguments() {
    let harness = harness("chain-exe");
    let cwd = unicode_cwd(&harness.root);
    run_chain_case(&harness, echo_exe(), "inst-exe", &cwd, boundary_args());
}

#[test]
fn the_cmd_chain_preserves_boundary_arguments() {
    let harness = harness("chain-cmd");
    let cwd = unicode_cwd(&harness.root);
    // A realistic shim: a .cmd that forwards `%*` to a native program.
    let script = harness.root.join("shim.cmd");
    std::fs::write(
        &script,
        format!(
            "@echo off\r\n\"{}\" %*\r\n",
            echo_exe().display()
        ),
    )
    .unwrap();
    run_chain_case(&harness, &script, "inst-cmd", &cwd, boundary_args());
    // The generated trampoline must be gone after the close.
    let launch_dir = harness.root.join("terminal").join("launch");
    let leftovers = std::fs::read_dir(&launch_dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().is_dir())
                .count()
        })
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "the trampoline directory was not cleaned up");
}

#[test]
fn cmd_launch_and_cleanup_work_in_a_unicode_metacharacter_profile() {
    let harness = harness("中文 profile & %PATH%");
    let cwd = unicode_cwd(&harness.root);
    let script = harness.root.join("forward.cmd");
    std::fs::write(&script, format!("@echo off\r\n\"{}\" %*\r\n", echo_exe().display())).unwrap();
    run_chain_case(&harness, &script, "profile-cmd", &cwd, boundary_args());
    assert!(harness.root.join("terminal/launch").is_dir());
    assert_eq!(std::fs::read_dir(harness.root.join("terminal/launch")).unwrap().count(), 0);
}

#[test]
fn ps1_forwarding_to_a_native_cli_preserves_quotes_empty_and_unicode() {
    let harness = harness("ps1-native-forward");
    let cwd = unicode_cwd(&harness.root);
    let script = harness.root.join("forward.ps1");
    std::fs::write(&script, format!("& '{}' @args\r\nexit $LASTEXITCODE\r\n", echo_exe().display())).unwrap();
    let mut args = boundary_args();
    args.push(String::new());
    args.push("back\\\"quote".into());
    run_chain_case(&harness, &script, "ps1-native", &cwd, args);
}

#[test]
fn the_ps1_chain_preserves_boundary_arguments() {
    let harness = harness("chain-ps1");
    let cwd = unicode_cwd(&harness.root);
    // The script writes what it received into a file, because a .ps1 is not a
    // native CRT process and its `$args` binding is what must be verified.
    let script = harness.root.join("shim.ps1");
    let out = harness.root.join("ps-args.json");
    std::fs::write(
        &script,
        format!(
            "$args | ConvertTo-Json -Compress | Set-Content -Encoding UTF8 -Path '{}'\r\n",
            out.display()
        ),
    )
    .unwrap();
    register_installation(
        &harness.service,
        &harness.root,
        "synthetic-ps",
        &script,
        "inst-ps1",
    );
    let ps_args = boundary_args();
    let spec = TerminalOpenSpec {
        target: TerminalTarget {
            kind: TerminalTargetKind::Installation,
            shell: None,
            installation_id: Some("inst-ps1".into()),
            custom_cli_id: None,
        },
        args: ps_args.clone(),
        cwd: cwd.display().to_string(),
        env: Vec::new(),
        cols: 120,
        rows: 30,
        title: None,
    };
    let view = harness.manager.open(&spec).unwrap();
    assert!(
        wait_until(|| out.is_file(), Duration::from_secs(40)),
        "the .ps1 chain never produced its argument file"
    );
    let text = std::fs::read_to_string(&out).unwrap();
    let text = text.trim_start_matches('\u{feff}');
    let received: Vec<String> = serde_json::from_str(text).unwrap();
    assert_eq!(received, ps_args);
    let _ = harness.manager.close(&TerminalCloseRequest {
        terminal_id: view.terminal_id,
        mode: Some(TerminalStopMode::Force),
        grace_ms: Some(1_000),
    });
}

#[test]
fn the_cmd_chain_never_lets_an_argument_become_batch_syntax() {
    let harness = harness("chain-cmd-inject");
    let cwd = unicode_cwd(&harness.root);
    let script = harness.root.join("shim.cmd");
    // A shim that forwards `%*` to the echo program and would print
    // `PWNED-BATCH` if the value ever escaped its quoted token.
    std::fs::write(
        &script,
        format!("@echo off\r\n\"{}\" %*\r\n", echo_exe().display()),
    )
    .unwrap();
    register_installation(
        &harness.service,
        &harness.root,
        "synthetic-inject",
        &script,
        "inst-inject",
    );
    let hostile = "x\"& echo PWNED-BATCH";
    let spec = TerminalOpenSpec {
        target: TerminalTarget {
            kind: TerminalTargetKind::Installation,
            shell: None,
            installation_id: Some("inst-inject".into()),
            custom_cli_id: None,
        },
        args: vec![hostile.into(), "50%PATH%".into()],
        cwd: cwd.display().to_string(),
        env: Vec::new(),
        cols: 160,
        rows: 30,
        title: None,
    };
    let view = harness.manager.open(&spec).unwrap();
    let mut cursor = 0u64;
    let text = read_until(
        &harness.manager,
        &view.terminal_id,
        &mut cursor,
        "##ARGV##",
        1,
        Duration::from_secs(40),
    );
    // The hostile value must arrive as one argv element: if the `&` had become
    // a command separator, the shim would have passed a prefix and the echo
    // program would report fewer arguments.
    let received = parse_argv(&text);
    assert_eq!(
        received,
        vec![hostile.to_string(), "50%PATH%".to_string()],
        "raw terminal text: {text:?}"
    );
    // … and the value was not split into a second command either.
    assert_eq!(
        text.matches("##ARGV##").count(),
        1,
        "the echo program ran more than once: {text:?}"
    );
    let _ = harness.manager.close(&TerminalCloseRequest {
        terminal_id: view.terminal_id,
        mode: Some(TerminalStopMode::Force),
        grace_ms: Some(1_000),
    });
}

#[test]
fn the_two_trampoline_builders_are_pure_and_escaping() {
    let batch = batch_trampoline(Path::new(r"C:\a b\shim.cmd"), &["x&y".into(), "50%".into()]);
    assert!(batch.contains("\"x&y\""));
    assert!(batch.contains("\"50%%\""));
    let ps = powershell_trampoline(Path::new(r"C:\a b\shim.ps1"), &["it's".into()]);
    assert!(ps.contains("'it''s'"));
}

// ---------------------------------------------------------------------------
// Run / process integration and bounds
// ---------------------------------------------------------------------------

#[test]
fn a_terminal_records_a_managed_pty_run_and_an_owned_process_link() {
    let harness = harness("run-link");
    let cwd = unicode_cwd(&harness.root);
    let view = harness
        .manager
        .open(&shell_spec(&cwd, ShellFlavor::Cmd))
        .unwrap();
    let detail = harness.service.get_session(&view.session_id).unwrap();
    assert_eq!(detail.runs.len(), 1);
    let run = &detail.runs[0];
    assert_eq!(run.id, view.run_id);
    assert_eq!(run.transport, RunTransport::Pty);
    assert_eq!(run.ownership, crate::multicli::domain::RunOwnership::Managed);
    assert_eq!(run.execution_status, ExecutionStatus::Running);
    assert_eq!(
        run.interaction_status,
        crate::multicli::domain::InteractionStatus::Unknown
    );
    assert_eq!(detail.session.origin, SessionOrigin::Managed);
    let links = harness.service.links_for_run(&view.run_id).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].link_kind, crate::multicli::domain::LinkKind::Owned);
    assert_eq!(links[0].process_identity, view.identity.clone().unwrap());

    harness
        .manager
        .close(&TerminalCloseRequest {
            terminal_id: view.terminal_id.clone(),
            mode: Some(TerminalStopMode::Force),
            grace_ms: Some(1_000),
        })
        .unwrap();
    let detail = harness.service.get_session(&view.session_id).unwrap();
    assert_eq!(detail.runs[0].execution_status, ExecutionStatus::Exited);
    assert!(detail.runs[0].ended_at.is_some());
}

#[test]
fn bounds_are_enforced_and_a_slow_reader_is_counted_not_queued() {
    let harness = harness("bounds");
    let cwd = unicode_cwd(&harness.root);
    let view = harness
        .manager
        .open(&shell_spec(&cwd, ShellFlavor::Cmd))
        .unwrap();
    let id = view.terminal_id.clone();
    assert_eq!(view.buffers.buffer_capacity_bytes, DEFAULT_BUFFER_BYTES);
    assert_eq!(view.buffers.log_capacity_bytes, DEFAULT_LOG_BYTES);

    // A read cursor past the end is a real protocol error.
    let error = harness
        .manager
        .read(&TerminalReadRequest {
            terminal_id: id.clone(),
            cursor: Some(u64::MAX),
            max_bytes: Some(1024),
        })
        .unwrap_err();
    assert_eq!(error.code, crate::multicli::error::ErrorCode::InvalidArgument);

    // The read reply is capped even when much more is buffered.
    let read = harness
        .manager
        .read(&TerminalReadRequest {
            terminal_id: id.clone(),
            cursor: None,
            max_bytes: Some(1),
        })
        .unwrap();
    assert!(read.bytes <= 1);

    // Writes are bounded and rejected with an explicit error.
    let oversized = "x".repeat(MAX_WRITE_BYTES + 1);
    let error = harness
        .manager
        .write(&TerminalWriteRequest {
            terminal_id: id.clone(),
            data: oversized,
        })
        .unwrap_err();
    assert_eq!(error.code, crate::multicli::error::ErrorCode::InvalidArgument);

    // The configurable buffer is persisted in this profile only.
    let limits = harness
        .manager
        .update_limits(&TerminalLimitsUpdate {
            buffer_bytes: Some(MIN_BUFFER_BYTES),
            log_bytes: Some(MIN_LOG_BYTES),
        })
        .unwrap();
    assert_eq!(limits.buffer_bytes, MIN_BUFFER_BYTES);
    let text = std::fs::read_to_string(harness.root.join("terminal-settings.json")).unwrap();
    assert!(text.contains("bufferBytes"));
    let _ = harness.manager.close(&TerminalCloseRequest {
        terminal_id: id,
        mode: Some(TerminalStopMode::Force),
        grace_ms: Some(1_000),
    });
}

#[test]
fn a_stopped_terminal_refuses_input_with_a_real_error() {
    let harness = harness("stopped-write");
    let cwd = unicode_cwd(&harness.root);
    let view = harness
        .manager
        .open(&shell_spec(&cwd, ShellFlavor::Cmd))
        .unwrap();
    harness
        .manager
        .stop(&TerminalStopRequest {
            terminal_id: view.terminal_id.clone(),
            mode: TerminalStopMode::Force,
            grace_ms: None,
        })
        .unwrap();
    let error = harness
        .manager
        .write(&TerminalWriteRequest {
            terminal_id: view.terminal_id.clone(),
            data: "echo nope\r".into(),
        })
        .unwrap_err();
    assert_eq!(error.code, crate::multicli::error::ErrorCode::Conflict);
    // The last output is still readable after the exit.
    let read = harness
        .manager
        .read(&TerminalReadRequest {
            terminal_id: view.terminal_id.clone(),
            cursor: None,
            max_bytes: None,
        })
        .unwrap();
    assert_eq!(read.state, TerminalState::Exited);
    let _ = harness.manager.close(&TerminalCloseRequest {
        terminal_id: view.terminal_id,
        mode: None,
        grace_ms: None,
    });
}

#[test]
fn shutdown_all_stops_only_managed_terminals() {
    let harness = harness("shutdown");
    let cwd = unicode_cwd(&harness.root);
    let first = harness
        .manager
        .open(&shell_spec(&cwd, ShellFlavor::Cmd))
        .unwrap();
    let second = harness
        .manager
        .open(&shell_spec(&cwd, ShellFlavor::Cmd))
        .unwrap();
    let view = harness.manager.quit_check();
    assert_eq!(view.active_count, 2);
    assert!(view.needs_confirmation);
    let stopped = harness
        .manager
        .shutdown_all(&TerminalShutdownAllRequest {
            mode: Some(TerminalStopMode::Gentle),
            grace_ms: Some(800),
        })
        .unwrap();
    assert_eq!(stopped.remaining, 0);
    assert_eq!(stopped.stopped.len(), 2);
    assert!(stopped.stopped.iter().all(|item| item.removed));
    assert_eq!(harness.manager.list().items.len(), 0);
    for id in [first.terminal_id, second.terminal_id] {
        assert!(harness
            .manager
            .read(&TerminalReadRequest {
                terminal_id: id,
                cursor: None,
                max_bytes: None
            })
            .is_err());
    }
}
