//! One managed terminal: ConPTY, child, job, reader thread, log and ring.
//!
//! Ownership rules that the whole lifecycle depends on:
//!
//! - the terminal owns exactly one ConPTY pair and one child tree;
//! - the child is put into a kill-on-close Job Object before it is reported as
//!   running, and a failed assignment aborts the creation instead of running
//!   without the tree constraint;
//! - the first `hostId + pid + creationTime` triple is captured right after
//!   spawn and re-checked before any termination, so a reused PID is refused;
//! - `finalize` is idempotent, which is what makes the stop/exit/reader-EOF
//!   race safe: whichever path arrives first wins, late events are absorbed.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::multicli::domain::{HostPath, ProcessIdentity};
use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::history::ids::iso_from_millis;

use super::identity::{self, IdentityCheck};
use super::launch::{cleanup_trampoline, LaunchPaths, LaunchPlan, PinnedTarget};
use super::logfile::TerminalLog;
use super::model::{
    JobGuardMode, JobGuardView, LaunchChain, TerminalBuffersView, TerminalExitView,
    TerminalLimits, TerminalNotify, TerminalReadView, TerminalState, TerminalStopMode,
    TerminalStopReport, TerminalTargetKind, TerminalView, TerminalWriteView, TERMINAL_SCHEMA_VERSION,
};
use super::ring::ByteRing;

pub type NotifyFn = Arc<dyn Fn(TerminalNotify) + Send + Sync>;
pub type FinishFn = Arc<dyn Fn(TerminalFinish) + Send + Sync>;

/// Reported once, when a terminal reaches a terminal state. The manager turns
/// this into the durable Run update.
#[derive(Debug, Clone)]
pub struct TerminalFinish {
    pub run_id: String,
    pub session_id: String,
    pub state: TerminalState,
    pub exit: Option<TerminalExitView>,
    pub reason: Option<String>,
    pub identity: Option<ProcessIdentity>,
}

pub struct SessionConfig {
    pub terminal_id: String,
    pub run_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub installation_id: Option<String>,
    pub title: String,
    pub product: Option<String>,
    pub plan: LaunchPlan,
    pub cwd: HostPath,
    pub args: Vec<String>,
    pub env_names: Vec<String>,
    /// Environment delta applied on top of the inherited environment.
    pub env: Vec<(String, String)>,
    pub limits: TerminalLimits,
    pub log: TerminalLog,
    pub cols: u16,
    pub rows: u16,
    pub started_at: String,
    pub launch_paths: Option<(LaunchPaths, String)>,
    pub notify: NotifyFn,
    pub finish: FinishFn,
}

pub struct SpawnFailure {
    pub error: AppError,
    /// `true` when a child existed and had to be cleaned up.
    pub child_started: bool,
}

struct Inner {
    state: TerminalState,
    exit: Option<TerminalExitView>,
    stop: Option<TerminalStopReport>,
    identity: Option<ProcessIdentity>,
    job: Option<Arc<crate::engine::job::KillOnCloseJob>>,
    job_view: JobGuardView,
    master: Option<Box<dyn MasterPty + Send>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    reader: Option<std::thread::JoinHandle<()>>,
    ended_at: Option<String>,
    notes: Vec<String>,
    dirty: bool,
    finished: bool,
}

pub struct TerminalSession {
    pub terminal_id: String,
    pub run_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub installation_id: Option<String>,
    pub title: String,
    pub product: Option<String>,
    pub target: PinnedTarget,
    pub plan: LaunchPlan,
    pub cwd: HostPath,
    pub args: Vec<String>,
    pub env_names: Vec<String>,
    pub started_at: String,
    pub stream_id: String,
    pub limits: TerminalLimits,
    inner: Mutex<Inner>,
    /// A visible exited state may precede the durable Run callback. Shutdown
    /// waits for this gate so app.exit cannot cut that callback off mid-commit.
    finalize_gate: Mutex<()>,
    /// Kept behind its own lock: a write that blocks on a full console pipe
    /// must never block a stop, a read or a state transition.
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    ring: Arc<Mutex<ByteRing>>,
    log: Arc<Mutex<TerminalLog>>,
    notify: NotifyFn,
    finish: FinishFn,
    launch_paths: Option<(LaunchPaths, String)>,
    closed: AtomicBool,
}

fn now_iso() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0);
    iso_from_millis(millis)
}

impl TerminalSession {
    pub fn spawn(config: SessionConfig) -> Result<Arc<Self>, SpawnFailure> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: config.rows,
                cols: config.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| SpawnFailure {
                error: AppError::new(
                    ErrorCode::Io,
                    format!("failed to open the pseudo console: {error}"),
                )
                .with_details(serde_json::json!({ "stage": "openpty" })),
                child_started: false,
            })?;

        let mut command = CommandBuilder::new(&config.plan.program);
        for arg in &config.plan.program_args {
            command.arg(arg);
        }
        if let Some(cwd) = config.cwd.canonical_path.as_deref() {
            command.cwd(cwd);
        } else {
            command.cwd(&config.cwd.display_path);
        }
        // A terminal needs a sane TERM for full-screen CLIs; a user-supplied
        // delta (applied below) still wins because `env` replaces the entry.
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        for (name, value) in &config.env {
            command.env(name, value);
        }
        // These values describe the launcher itself, never a user command.
        // Apply them last so an environment delta cannot replace the script.
        for (name, value) in &config.plan.launcher_env {
            command.env(name, value);
        }

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| SpawnFailure {
                error: AppError::new(
                    ErrorCode::Io,
                    format!("failed to start the terminal target: {error}"),
                )
                .with_details(serde_json::json!({
                    "stage": "spawn",
                    "chain": config.plan.target.chain.as_str(),
                })),
                child_started: false,
            })?;
        drop(pair.slave);

        let pid = match child.process_id() {
            Some(pid) => pid,
            None => {
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                return Err(SpawnFailure {
                    error: AppError::new(
                        ErrorCode::Io,
                        "the started process has no PID; ownership cannot be proven",
                    )
                    .with_details(serde_json::json!({ "stage": "spawnPid" })),
                    child_started: true,
                });
            }
        };

        #[cfg(windows)]
        let job = match crate::engine::job::job_for_pid(pid) {
            Ok(job) => job,
            Err(failure) => {
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                return Err(SpawnFailure {
                    error: AppError::new(
                        ErrorCode::Unsupported,
                        "the owned process tree could not be constrained; the terminal was not started",
                    )
                    .with_details(serde_json::json!({
                        "stage": "jobAssignment",
                        "jobStage": failure.stage,
                        "reason": failure.reason,
                        "pid": pid,
                    })),
                    child_started: true,
                });
            }
        };
        #[cfg(not(windows))]
        let job = {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            return Err(SpawnFailure {
                error: AppError::new(
                    ErrorCode::Unsupported,
                    "managed terminals require Windows Job Objects",
                )
                .with_details(serde_json::json!({ "stage": "jobAssignment" })),
                child_started: true,
            });
        };

        let identity = match identity::capture("local-windows", pid) {
            Ok(identity) => identity,
            Err(error) => {
                job.terminate();
                let mut child = child;
                let _ = child.wait();
                return Err(SpawnFailure {
                    error,
                    child_started: true,
                });
            }
        };

        let reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => {
                job.terminate();
                let mut child = child;
                let _ = child.wait();
                return Err(SpawnFailure {
                    error: AppError::new(
                        ErrorCode::Io,
                        format!("failed to attach the console reader: {error}"),
                    )
                    .with_details(serde_json::json!({ "stage": "cloneReader" })),
                    child_started: true,
                });
            }
        };
        let writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(error) => {
                job.terminate();
                let mut child = child;
                let _ = child.wait();
                return Err(SpawnFailure {
                    error: AppError::new(
                        ErrorCode::Io,
                        format!("failed to attach the console writer: {error}"),
                    )
                    .with_details(serde_json::json!({ "stage": "takeWriter" })),
                    child_started: true,
                });
            }
        };

        let ring = Arc::new(Mutex::new(ByteRing::new(
            config.limits.buffer_bytes as usize,
        )));
        let log = Arc::new(Mutex::new(config.log));

        let session = Arc::new(Self {
            terminal_id: config.terminal_id.clone(),
            run_id: config.run_id.clone(),
            session_id: config.session_id.clone(),
            agent_id: config.agent_id.clone(),
            installation_id: config.installation_id.clone(),
            title: config.title.clone(),
            product: config.product.clone(),
            target: config.plan.target.clone(),
            plan: config.plan.clone(),
            cwd: config.cwd.clone(),
            args: config.args.clone(),
            env_names: config.env_names.clone(),
            started_at: config.started_at.clone(),
            stream_id: format!("terminal/{}", config.terminal_id),
            limits: config.limits.clone(),
            writer: Mutex::new(Some(writer)),
            finalize_gate: Mutex::new(()),
            inner: Mutex::new(Inner {
                state: TerminalState::Starting,
                exit: None,
                stop: None,
                identity: Some(identity.clone()),
                job: Some(job),
                job_view: JobGuardView {
                    mode: JobGuardMode::KillOnClose,
                    assigned: true,
                    reason: None,
                },
                master: Some(pair.master),
                child: Some(child),
                reader: None,
                ended_at: None,
                notes: config.plan.notes.clone(),
                dirty: false,
                finished: false,
            }),
            ring: Arc::clone(&ring),
            log: Arc::clone(&log),
            notify: Arc::clone(&config.notify),
            finish: Arc::clone(&config.finish),
            launch_paths: config.launch_paths.clone(),
            closed: AtomicBool::new(false),
        });

        {
            let mut inner = session.inner.lock();
            inner.state = TerminalState::Running;
        }
        session.emit_notify(TerminalState::Running);

        let weak = Arc::downgrade(&session);
        let reader_ring = Arc::clone(&ring);
        let reader_log = Arc::clone(&log);
        let handle = std::thread::Builder::new()
            .name(format!("multicli-pty-{}", session.terminal_id))
            .spawn(move || drain_reader(weak, reader, reader_ring, reader_log))
            .map_err(|error| {
                // The child exists and is owned; ending it here is the only
                // honest outcome, since no reader means no output at all.
                session.force_end_owned_tree();
                SpawnFailure {
                    error: AppError::new(
                        ErrorCode::Io,
                        format!("failed to start the console reader thread: {error}"),
                    )
                    .with_details(serde_json::json!({ "stage": "spawnReader" })),
                    child_started: true,
                }
            })?;
        session.inner.lock().reader = Some(handle);
        // Exit watchdog. A ConPTY does not always deliver EOF the moment the
        // child ends (the console host can keep the output pipe open), so the
        // reader alone is not a reliable "the process is gone" signal. Polling
        // the child handle is cheap and makes a natural exit visible.
        session.start_watchdog();
        Ok(session)
    }

    fn start_watchdog(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name(format!("multicli-pty-watch-{}", self.terminal_id))
            .spawn(move || loop {
                let Some(session) = weak.upgrade() else {
                    return;
                };
                if session.closed.load(Ordering::SeqCst) || session.state().is_terminal() {
                    return;
                }
                if let Some(exit) = session.poll_exit() {
                    session.finalize(TerminalState::Exited, Some(exit), None);
                    return;
                }
                drop(session);
                std::thread::sleep(Duration::from_millis(150));
            });
        if let Err(error) = spawned {
            // Without the watchdog a natural exit is only visible through the
            // console EOF, which ConPTY does not always deliver. That is a
            // degraded mode, so it is recorded instead of silently ignored.
            eprintln!("[multicli] terminal exit watchdog unavailable: {error}");
            self.inner
                .lock()
                .notes
                .push("退出看门狗启动失败，只能依赖控制台 EOF 判断退出".into());
        }
    }

    /// One non-blocking look at the child handle.
    fn poll_exit(&self) -> Option<TerminalExitView> {
        let mut inner = self.inner.lock();
        let child = inner.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => Some(exit_from_status(&status)),
            _ => None,
        }
    }

    fn emit_notify(&self, state: TerminalState) {
        let (buffered, dropped, end) = {
            let ring = self.ring.lock();
            (ring.buffered_bytes(), ring.dropped_bytes(), ring.end())
        };
        (self.notify)(TerminalNotify {
            kind: "state".into(),
            terminal_id: self.terminal_id.clone(),
            run_id: self.run_id.clone(),
            stream_id: self.stream_id.clone(),
            seq: end,
            state,
            buffered_bytes: buffered,
            dropped_bytes: dropped,
        });
    }

    /// Called by the reader thread for every batch. Only the first batch after
    /// a read flips the flag, so a slow WebView can hold at most one pending
    /// notification per terminal.
    fn mark_dirty(&self) {
        let promoted = {
            let mut inner = self.inner.lock();
            if inner.dirty {
                false
            } else {
                inner.dirty = true;
                true
            }
        };
        if promoted {
            let (buffered, dropped, end) = {
                let ring = self.ring.lock();
                (ring.buffered_bytes(), ring.dropped_bytes(), ring.end())
            };
            (self.notify)(TerminalNotify {
                kind: "output".into(),
                terminal_id: self.terminal_id.clone(),
                run_id: self.run_id.clone(),
                stream_id: self.stream_id.clone(),
                seq: end,
                state: self.state(),
                buffered_bytes: buffered,
                dropped_bytes: dropped,
            });
        }
    }

    pub fn state(&self) -> TerminalState {
        self.inner.lock().state
    }

    pub fn is_terminal(&self) -> bool {
        self.state().is_terminal()
    }

    pub fn exit(&self) -> Option<TerminalExitView> {
        self.inner.lock().exit.clone()
    }

    pub fn read(&self, cursor: Option<u64>, limit: usize) -> Result<TerminalReadView, AppError> {
        let outcome = {
            let ring = self.ring.lock();
            ring.read(cursor, limit)
                .map_err(|reason| AppError::invalid_argument("cursor", reason))?
        };
        {
            let mut inner = self.inner.lock();
            inner.dirty = false;
        }
        let (state, exit) = {
            let inner = self.inner.lock();
            (inner.state, inner.exit.clone())
        };
        Ok(TerminalReadView {
            terminal_id: self.terminal_id.clone(),
            run_id: self.run_id.clone(),
            stream_id: self.stream_id.clone(),
            from_seq: outcome.from_seq,
            to_seq: outcome.to_seq,
            text: outcome.text,
            bytes: outcome.bytes,
            dropped_before: outcome.dropped_before,
            dropped_bytes: outcome.dropped_bytes,
            // A first read (`cursor=null`) that starts after a trimmed window is
            // also a truncation, even though nothing was dropped *since* the
            // caller's cursor: there is no cursor yet.
            history_truncated: outcome.dropped_before > 0
                || (cursor.is_none() && outcome.from_seq > 0),
            buffered_bytes: outcome.buffered_bytes,
            state,
            exit,
        })
    }

    pub fn write(&self, data: &str) -> Result<TerminalWriteView, AppError> {
        let state = {
            let inner = self.inner.lock();
            if inner.state != TerminalState::Running {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "the terminal is not running; input was not delivered",
                )
                .with_details(serde_json::json!({
                    "stage": "write",
                    "state": inner.state.as_str(),
                })));
            }
            inner.state
        };
        {
            let mut guard = self.writer.lock();
            let Some(writer) = guard.as_mut() else {
                return Err(AppError::new(
                    ErrorCode::Io,
                    "the terminal writer is no longer available",
                )
                .with_details(serde_json::json!({ "stage": "write" })));
            };
            writer
                .write_all(data.as_bytes())
                .and_then(|()| writer.flush())
                .map_err(|error| {
                    AppError::new(
                        ErrorCode::Io,
                        format!("failed to write to the terminal: {error}"),
                    )
                    .with_details(serde_json::json!({ "stage": "write" }))
                })?;
        }
        Ok(TerminalWriteView {
            terminal_id: self.terminal_id.clone(),
            accepted_bytes: data.len() as u64,
            state,
        })
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), AppError> {
        let inner = self.inner.lock();
        let Some(master) = inner.master.as_ref() else {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "the terminal console is already closed",
            )
            .with_details(serde_json::json!({ "stage": "resize" })));
        };
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| {
                AppError::new(
                    ErrorCode::Io,
                    format!("failed to resize the terminal: {error}"),
                )
                .with_details(serde_json::json!({ "stage": "resize" }))
            })
    }

    /// Sends Ctrl+C without ever blocking behind a large paste: when the writer
    /// is busy, the interrupt is reported as not sent and the caller falls back
    /// to the timeout/owned-tree path.
    fn send_interrupt(&self) -> bool {
        let Some(mut guard) = self.writer.try_lock() else {
            return false;
        };
        let Some(writer) = guard.as_mut() else {
            return false;
        };
        writer.write_all(&[0x03]).is_ok() && writer.flush().is_ok()
    }

    /// Polls the child without holding the lock between polls.
    fn wait_for_exit(&self, timeout: Duration) -> Option<TerminalExitView> {
        let deadline = Instant::now() + timeout;
        loop {
            let exited = {
                let mut inner = self.inner.lock();
                match inner.child.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => Some(exit_from_status(&status)),
                        Ok(None) => None,
                        Err(_) => None,
                    },
                    None => inner.exit.clone(),
                }
            };
            if exited.is_some() {
                return exited;
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn force_end_owned_tree(&self) {
        let mut inner = self.inner.lock();
        if let Some(job) = inner.job.as_ref() {
            job.terminate();
        } else if let Some(child) = inner.child.as_mut() {
            let _ = child.kill();
        }
    }

    /// Idempotent terminal transition. The first caller wins; every later
    /// caller (reader EOF after a stop, repeated stop, late kill) is absorbed.
    fn finalize(
        &self,
        state: TerminalState,
        exit: Option<TerminalExitView>,
        reason: Option<String>,
    ) -> bool {
        let _finalize = self.finalize_gate.lock();
        let (changed, finish) = {
            let mut inner = self.inner.lock();
            if inner.state.is_terminal() || inner.finished {
                return false;
            }
            inner.state = state;
            inner.ended_at = Some(now_iso());
            if exit.is_some() {
                inner.exit = exit.clone();
            }
            if let Some(reason) = &reason {
                inner.notes.push(reason.clone());
            }
            inner.finished = true;
            let finish = TerminalFinish {
                run_id: self.run_id.clone(),
                session_id: self.session_id.clone(),
                state,
                exit: inner.exit.clone(),
                reason,
                identity: inner.identity.clone(),
            };
            (true, finish)
        };
        if changed {
            self.emit_notify(state);
            (self.finish)(finish);
            // The console and the job are released only after the durable
            // record is written, so a crash between the two cannot leave an
            // unrecorded tree. The writer is taken with `try_lock`: a big paste
            // may still be inside `write_all`, and finalisation must not wait
            // for it. A writer that cannot be taken is released with the
            // session.
            if let Some(mut guard) = self.writer.try_lock() {
                *guard = None;
            }
            let mut inner = self.inner.lock();
            inner.master = None;
            if let Some(job) = inner.job.take() {
                // Kill-on-close: anything still alive in the owned tree dies
                // here, including grandchildren the shell left behind.
                drop(job);
            }
        }
        true
    }

    fn on_reader_end(weak: &Weak<TerminalSession>, read_error: Option<String>) {
        let Some(session) = weak.upgrade() else {
            return;
        };
        if session.state().is_terminal() {
            return;
        }
        if let Some(reason) = read_error {
            // A read failure with a live child would leave an ownerless tree, so
            // the tree is ended first; the exit status then decides whether this
            // is an abnormal end or just a console that closed after a clean
            // exit with a leftover error.
            session.force_end_owned_tree();
            let exit = session.wait_for_exit(Duration::from_secs(2));
            let clean = exit.as_ref().map(|value| value.success).unwrap_or(false);
            let state = if clean {
                TerminalState::Exited
            } else {
                TerminalState::Failed
            };
            session.finalize(
                state,
                exit,
                Some(format!("console reader ended with an error: {reason}")),
            );
            return;
        }
        let exit = session.wait_for_exit(Duration::from_secs(2));
        let success = exit.as_ref().map(|value| value.success).unwrap_or(false);
        let state = if success {
            TerminalState::Exited
        } else {
            // A non-zero exit is a normal terminal state, not a failure of the
            // application: the CLI reported its own status.
            TerminalState::Exited
        };
        session.finalize(state, exit, None);
    }

    pub fn stop(&self, mode: TerminalStopMode, grace: Duration) -> TerminalStopReport {
        let started = Instant::now();
        let state = self.state();
        if state.is_terminal() {
            let _committed = self.finalize_gate.lock();
            return TerminalStopReport {
                requested: mode,
                interrupt_sent: false,
                escalated: false,
                waited_ms: 0,
                already_terminal: true,
                refused_reason: None,
            };
        }
        if state == TerminalState::Stopping && mode != TerminalStopMode::Force {
            return TerminalStopReport {
                requested: mode,
                interrupt_sent: false,
                escalated: false,
                waited_ms: 0,
                already_terminal: false,
                refused_reason: Some("a stop is already in progress".into()),
            };
        }
        if mode == TerminalStopMode::Interrupt {
            let interrupt_sent = self.send_interrupt();
            let report = TerminalStopReport {
                requested: mode,
                interrupt_sent,
                escalated: false,
                waited_ms: started.elapsed().as_millis() as u64,
                already_terminal: false,
                refused_reason: if interrupt_sent {
                    None
                } else {
                    Some("the console writer is gone".into())
                },
            };
            let mut inner = self.inner.lock();
            inner.stop = Some(report.clone());
            return report;
        }

        {
            let mut inner = self.inner.lock();
            inner.state = TerminalState::Stopping;
        }
        self.emit_notify(TerminalState::Stopping);
        let mut interrupt_sent = false;
        if mode == TerminalStopMode::Gentle {
            interrupt_sent = self.send_interrupt();
            if let Some(exit) = self.wait_for_exit(grace) {
                self.finalize(TerminalState::Exited, Some(exit), None);
                let report = TerminalStopReport {
                    requested: mode,
                    interrupt_sent,
                    escalated: false,
                    waited_ms: started.elapsed().as_millis() as u64,
                    already_terminal: false,
                    refused_reason: None,
                };
                self.inner.lock().stop = Some(report.clone());
                return report;
            }
        }

        // Identity is re-checked before the tree is touched: a PID that now
        // belongs to another process must never be terminated.
        let identity = self.inner.lock().identity.clone();
        if let Some(identity) = identity {
            match identity::check(&identity) {
                IdentityCheck::Reused => {
                    let mut inner = self.inner.lock();
                    inner.state = TerminalState::Running;
                    let report = TerminalStopReport {
                        requested: mode,
                        interrupt_sent,
                        escalated: false,
                        waited_ms: started.elapsed().as_millis() as u64,
                        already_terminal: false,
                        refused_reason: Some(
                            "pid now belongs to a different process; nothing was terminated".into(),
                        ),
                    };
                    inner.stop = Some(report.clone());
                    drop(inner);
                    self.emit_notify(TerminalState::Running);
                    return report;
                }
                IdentityCheck::Unknown { reason } => {
                    let mut inner = self.inner.lock();
                    inner.notes.push(format!("identity check was inconclusive: {reason}"));
                }
                IdentityCheck::Same | IdentityCheck::Gone => {}
            }
        }

        self.force_end_owned_tree();
        let exit = self.wait_for_exit(Duration::from_secs(2));
        self.finalize(TerminalState::Exited, exit, None);
        let report = TerminalStopReport {
            requested: mode,
            interrupt_sent,
            escalated: mode == TerminalStopMode::Gentle,
            waited_ms: started.elapsed().as_millis() as u64,
            already_terminal: false,
            refused_reason: None,
        };
        self.inner.lock().stop = Some(report.clone());
        report
    }

    /// Called from the application exit path. Ends the owned tree without
    /// waiting; the reader thread and the job handle are still cleaned up by
    /// their own drop paths.
    pub fn hard_shutdown(&self) {
        if self.state().is_terminal() {
            let _committed = self.finalize_gate.lock();
            return;
        }
        self.force_end_owned_tree();
        self.finalize(TerminalState::Exited, None, Some("application exit".into()));
    }

    /// Releases everything this terminal owns and removes its trampoline.
    pub fn release(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.hard_shutdown();
        let handle = self.inner.lock().reader.take();
        if let Some(handle) = handle {
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                // A blocked read is released by the closed console; poll
                // briefly instead of hanging an IPC reply on a dead process.
                for _ in 0..40 {
                    if handle.is_finished() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                if handle.is_finished() {
                    let _ = handle.join();
                }
            }
        }
        if let Some((paths, terminal_id)) = &self.launch_paths {
            let _ = cleanup_trampoline(paths, terminal_id);
        }
    }

    /// Application-exit variant: ends the owned tree and drops the handles, but
    /// never waits for a reader thread. At exit the process is about to end, and
    /// sixteen terminals must not add sixteen seconds to shutdown.
    pub fn release_without_join(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.hard_shutdown();
        let _ = self.inner.lock().reader.take();
        if let Some((paths, terminal_id)) = &self.launch_paths {
            let _ = cleanup_trampoline(paths, terminal_id);
        }
    }

    pub fn view(&self) -> TerminalView {
        let (state, exit, stop, identity, job_view, ended_at, mut notes) = {
            let inner = self.inner.lock();
            (
                inner.state,
                inner.exit.clone(),
                inner.stop.clone(),
                inner.identity.clone(),
                inner.job_view.clone(),
                inner.ended_at.clone(),
                inner.notes.clone(),
            )
        };
        let (buffered, dropped, capacity) = {
            let ring = self.ring.lock();
            (
                ring.buffered_bytes(),
                ring.dropped_bytes(),
                ring.capacity() as u64,
            )
        };
        let (log_path, log_written, log_rotations, log_dropped, log_cap) = {
            let log = self.log.lock();
            if let Some(error) = log.last_error() {
                notes.push(error.to_string());
            }
            (
                log.path().display().to_string(),
                log.written(),
                log.rotations(),
                log.dropped(),
                log.capacity(),
            )
        };
        TerminalView {
            schema_version: TERMINAL_SCHEMA_VERSION,
            terminal_id: self.terminal_id.clone(),
            run_id: self.run_id.clone(),
            session_id: self.session_id.clone(),
            agent_id: self.agent_id.clone(),
            installation_id: self.installation_id.clone(),
            title: self.title.clone(),
            target_kind: if self.installation_id.is_some() {
                TerminalTargetKind::Installation
            } else {
                TerminalTargetKind::Shell
            },
            product: self.product.clone(),
            executable: self.target.host_path("local-windows"),
            launch_chain: self.target.chain,
            launch_argv: self.plan.argv_preview.clone(),
            args: self.args.clone(),
            cwd: self.cwd.clone(),
            env_names: self.env_names.clone(),
            state,
            interaction_status: "unknown".into(),
            identity,
            job: job_view,
            buffers: TerminalBuffersView {
                buffer_capacity_bytes: capacity,
                buffered_bytes: buffered,
                dropped_bytes: dropped,
                log_path,
                log_capacity_bytes: log_cap,
                log_bytes: log_written,
                log_rotations,
                log_dropped_bytes: log_dropped,
            },
            exit,
            stop,
            started_at: self.started_at.clone(),
            ended_at,
            evidence: format!(
                "multicli-terminal-v1: owned ConPTY {} process, {} chain, job kill-on-close",
                match state {
                    TerminalState::Running | TerminalState::Starting => "live",
                    _ => "ended",
                },
                self.target.chain.as_str()
            ),
            notes,
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // The job handle lives in `inner`; dropping it kills the owned tree, so
        // a leaked session can never leave an orphan family behind.
    }
}

fn exit_from_status(status: &portable_pty::ExitStatus) -> TerminalExitView {
    let success = status.success();
    let code = i32::try_from(status.exit_code()).ok();
    TerminalExitView {
        code,
        success,
        ended_at: now_iso(),
        reported: true,
        signal: if success {
            None
        } else {
            Some(format!("exit-code-{}", status.exit_code()))
        },
    }
}

fn drain_reader(
    weak: Weak<TerminalSession>,
    mut reader: Box<dyn Read + Send>,
    ring: Arc<Mutex<ByteRing>>,
    log: Arc<Mutex<TerminalLog>>,
) {
    let mut buffer = [0u8; 8192];
    let mut read_error: Option<String> = None;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                {
                    let mut ring = ring.lock();
                    ring.push(&buffer[..count]);
                }
                {
                    let mut log = log.lock();
                    log.write(&buffer[..count]);
                }
                if let Some(session) = weak.upgrade() {
                    session.mark_dirty();
                } else {
                    break;
                }
            }
            Err(error) => {
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                read_error = Some(error.to_string());
                break;
            }
        }
    }
    drop(reader);
    log.lock().finish();
    TerminalSession::on_reader_end(&weak, read_error);
}

/// Kept next to the chain enum so a future chain cannot be added without a
/// compile error here.
pub fn chain_note(chain: LaunchChain) -> &'static str {
    match chain {
        LaunchChain::Exe => "exe 链：argv 由 CreateProcess 按 MSVC 规则引用，无 shell 参与。",
        LaunchChain::Cmd => "cmd 链：经自有 trampoline，参数不进入用户可编辑的 shell 行。",
        LaunchChain::Powershell => "ps1 链：经自有 trampoline，-File 不解析用户参数。",
    }
}

#[cfg(all(test, windows))]
mod finalization_tests {
    use super::*;
    use super::super::launch::{plan_launch, shell_path};
    use super::super::model::ShellFlavor;
    use std::sync::mpsc;

    #[test]
    fn stop_waits_for_durable_finalizer_in_another_thread() {
        let root = std::env::temp_dir().join(format!("multicli-finalize-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let target = PinnedTarget {
            kind: TerminalTargetKind::Shell, label: "synthetic finalization race".into(),
            product: None, agent_id: "synthetic".into(), installation_id: None,
            executable: shell_path(ShellFlavor::Cmd).unwrap(), chain: LaunchChain::Exe,
            shell_flavor: Some(ShellFlavor::Cmd), version: None,
        };
        let paths = LaunchPaths::resolve(&root).unwrap();
        let plan = plan_launch(&target, &[], "race", &paths).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let session = TerminalSession::spawn(SessionConfig {
            terminal_id: "race".into(), run_id: "synthetic-run".into(), session_id: "synthetic-session".into(),
            agent_id: "synthetic".into(), installation_id: None, title: "race".into(), product: None,
            plan, cwd: HostPath { host_id: "local-windows".into(), display_path: root.display().to_string(), canonical_path: Some(root.display().to_string()) },
            args: vec![], env_names: vec![], env: vec![], limits: TerminalLimits::default(),
            log: TerminalLog::create(&root, "race", 1024*1024, "").unwrap(),
            cols: 80, rows: 24, started_at: now_iso(), launch_paths: None,
            notify: Arc::new(|_|{}), finish: Arc::new(move |_| {
                entered_tx.send(()).unwrap();
                release_rx.lock().recv_timeout(Duration::from_secs(10)).unwrap();
            }),
        }).unwrap_or_else(|failure| panic!("{}", failure.error));
        let finalizing = Arc::clone(&session);
        let worker = std::thread::spawn(move || finalizing.finalize(TerminalState::Exited, None, None));
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let stopping = Arc::clone(&session);
        let stopper = std::thread::spawn(move || {
            stopping.stop(TerminalStopMode::Force, Duration::from_millis(100));
            stopped_tx.send(()).unwrap();
        });
        let returned_before_commit = stopped_rx.recv_timeout(Duration::from_millis(150)).is_ok();
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        stopper.join().unwrap();
        session.release();
        assert!(!returned_before_commit, "stop returned while another thread was still persisting the terminal Run; app exit can kill that commit");
    }
}
