//! An owned CLI process speaking a line protocol over stdio.
//!
//! Ownership rules (the same ones S04 established for PTYs):
//!
//! - the child is created by this application and immediately placed in a
//!   kill-on-close Job Object; a failed assignment ends the child instead of
//!   running an unconstrained tree;
//! - `hostId + pid + creationTime` is captured right after spawn and re-checked
//!   before any termination, so a recycled PID is never signalled;
//! - the process is only ever addressable through this handle. There is no
//!   "attach to a PID" entry point, so an externally started CLI can never be
//!   stopped from here.

use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::multicli::domain::ProcessIdentity;
use crate::multicli::error::{AppError, ErrorCode};

use super::model::MAX_PROTOCOL_LINE_BYTES;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `CREATE_NO_WINDOW`: a protocol child has no console, so spawning it must not
/// flash a window or attach to the caller's console.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct ProcessSpec {
    pub program: std::path::PathBuf,
    pub args: Vec<String>,
    pub cwd: std::path::PathBuf,
    /// Process-scoped environment delta. Never a global change.
    pub env: Vec<(String, String)>,
    /// Names removed from the inherited environment. CodeBuddy's ACP entry
    /// point crashes on an inherited `SERVER__PORT` the host already occupies;
    /// removing it here is a process-local fix, not a configuration edit.
    pub env_remove: Vec<String>,
}

pub struct OwnedProcess {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<std::process::ChildStdin>>,
    pub pid: u32,
    pub identity: ProcessIdentity,
    job: Option<Arc<crate::engine::job::KillOnCloseJob>>,
    exited: AtomicBool,
    /// stderr is drained so a chatty CLI cannot block on a full pipe.
    stderr_tail: Arc<Mutex<Vec<String>>>,
    stderr_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl OwnedProcess {
    pub fn spawn(host_id: &str, spec: &ProcessSpec) -> Result<(Self, ChildStdout), AppError> {
        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        command.current_dir(&spec.cwd);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        for (name, value) in &spec.env {
            command.env(name, value);
        }
        for name in &spec.env_remove {
            command.env_remove(name);
        }
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);

        let mut child = command.spawn().map_err(|error| {
            AppError::new(
                ErrorCode::Io,
                format!("failed to start the CLI: {error}"),
            )
            .with_details(serde_json::json!({
                "stage": "spawn",
                "program": spec.program.display().to_string(),
            }))
            .with_retryable(true)
        })?;

        let pid = match child.id() {
            0 => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AppError::new(
                    ErrorCode::Io,
                    "the started process reported no PID; ownership cannot be proven",
                )
                .with_details(serde_json::json!({ "stage": "spawnPid" })));
            }
            pid => pid,
        };

        #[cfg(windows)]
        let job = match crate::engine::job::job_for_pid(pid) {
            Ok(job) => Some(job),
            Err(failure) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AppError::new(
                    ErrorCode::Unsupported,
                    "the owned process tree could not be constrained; the run was not started",
                )
                .with_details(serde_json::json!({
                    "stage": "jobAssignment",
                    "jobStage": failure.stage,
                    "reason": failure.reason,
                    "pid": pid,
                })));
            }
        };
        #[cfg(not(windows))]
        let job: Option<Arc<crate::engine::job::KillOnCloseJob>> = {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "the structured runtime requires Windows Job Objects",
            )
            .with_details(serde_json::json!({ "stage": "jobAssignment" })));
        };

        let identity = super::super::terminal::identity::capture(host_id, pid).map_err(|error| {
            let _ = child.kill();
            let _ = child.wait();
            error
        })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            AppError::new(ErrorCode::Io, "the CLI produced no stdout pipe")
                .with_details(serde_json::json!({ "stage": "stdout" }))
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            AppError::new(ErrorCode::Io, "the CLI produced no stdin pipe")
                .with_details(serde_json::json!({ "stage": "stdin" }))
        })?;

        let stderr_tail = Arc::new(Mutex::new(Vec::new()));
        let stderr_thread = child.stderr.take().map(|stream| {
            let tail = Arc::clone(&stderr_tail);
            std::thread::Builder::new()
                .name("multicli-runtime-stderr".into())
                .spawn(move || drain_stderr(stream, tail))
                .ok()
        });

        let process = Self {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(stdin)),
            pid,
            identity,
            job,
            exited: AtomicBool::new(false),
            stderr_tail,
            stderr_thread: Mutex::new(stderr_thread.flatten()),
        };
        Ok((process, stdout))
    }

    /// Writes one already-serialised JSON line. The newline is added here so a
    /// caller cannot accidentally emit a half frame.
    pub fn write_line(&self, line: &str) -> Result<(), AppError> {
        let mut guard = self.stdin.lock();
        let Some(stdin) = guard.as_mut() else {
            // Nothing was written, so answering is still possible if the
            // request is retried. The retry path keys on "Io + retryable",
            // which is the only combination that may re-send a decision: a
            // write that failed midway stays non-retryable because a second
            // attempt could double-grant.
            return Err(AppError::new(
                ErrorCode::Io,
                "the CLI stdin is already closed; nothing was written",
            )
            .with_details(serde_json::json!({ "stage": "write" }))
            .with_retryable(true));
        };
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .map_err(|error| {
                AppError::new(
                    ErrorCode::Io,
                    format!("failed to write to the CLI: {error}"),
                )
                .with_details(serde_json::json!({ "stage": "write", "pid": self.pid }))
                .with_retryable(false)
            })
    }

    pub fn close_stdin(&self) {
        let mut guard = self.stdin.lock();
        if let Some(mut stdin) = guard.take() {
            let _ = stdin.flush();
        }
    }

    pub fn try_wait(&self) -> Option<i32> {
        let mut guard = self.child.lock();
        let child = guard.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => {
                self.exited.store(true, Ordering::SeqCst);
                Some(status.code().unwrap_or(-1))
            }
            _ => None,
        }
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// Ends the owned tree. The job object is the primary mechanism (it also
    /// catches grandchildren); `kill` is the fallback for a failed assignment,
    /// which cannot happen because a failed assignment aborts the spawn.
    pub fn kill_tree(&self) {
        if let Some(job) = &self.job {
            job.terminate();
        }
        let mut guard = self.child.lock();
        if let Some(child) = guard.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.exited.store(true, Ordering::SeqCst);
        self.close_stdin();
    }

    /// Waits for a natural exit, bounded. Returns the exit code when the child
    /// ended inside the window.
    pub fn wait_timeout(&self, limit: Duration) -> Option<i32> {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(code) = self.try_wait() {
                return Some(code);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn recheck_identity(&self, host_id: &str) -> bool {
        let _ = host_id;
        matches!(
            super::super::terminal::identity::check(&self.identity),
            super::super::terminal::identity::IdentityCheck::Same
        )
    }

    /// Last stderr lines, for a failure report. Bounded and never includes a
    /// prompt: the adapters never write user text to stderr.
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail.lock().clone()
    }

    pub fn join_stderr(&self) {
        if let Some(handle) = self.stderr_thread.lock().take() {
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
impl OwnedProcess {
    /// A handle with no child, for unit tests that only exercise request
    /// bookkeeping. Writing to it returns an error instead of panicking, so a
    /// test can never silently "send" a frame.
    pub(crate) fn detached_for_tests() -> Self {
        Self {
            child: Mutex::new(None),
            stdin: Mutex::new(None),
            pid: 0,
            identity: ProcessIdentity {
                host_id: "local-windows".into(),
                pid: 0,
                creation_time: "1970-01-01T00:00:00.0000000Z".into(),
            },
            job: None,
            exited: AtomicBool::new(false),
            stderr_tail: Arc::new(Mutex::new(Vec::new())),
            stderr_thread: Mutex::new(None),
        }
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // Never leave an owned tree behind, and never signal a PID that might
        // have been recycled: the job handle already died with this struct, so
        // `KillOnCloseJob::drop` is what actually ends the tree.
        self.close_stdin();
        let mut guard = self.child.lock();
        if let Some(child) = guard.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

fn drain_stderr(mut stream: std::process::ChildStderr, tail: Arc<Mutex<Vec<String>>>) {
    let mut reader = BufReader::new(&mut stream);
    let mut buffer = [0u8; 4096];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                pending.extend_from_slice(&buffer[..count]);
                while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = pending.drain(..=index).collect();
                    record_stderr_line(&tail, &line);
                }
                if pending.len() > 64 * 1024 {
                    // A line that never ends must not grow without bound.
                    record_stderr_line(&tail, &pending);
                    pending.clear();
                }
            }
            Err(_) => break,
        }
    }
    if !pending.is_empty() {
        record_stderr_line(&tail, &pending);
    }
}

fn record_stderr_line(tail: &Arc<Mutex<Vec<String>>>, line: &[u8]) {
    let text = String::from_utf8_lossy(line);
    let text = redact_stderr_line(&text);
    if text.is_empty() {
        return;
    }
    let mut guard = tail.lock();
    guard.push(text);
    if guard.len() > 40 {
        guard.remove(0);
    }
}

/// stderr is kept only to explain a failure, so a long line is truncated and
/// anything that looks like a credential or a URL is replaced. A CLI banner may
/// contain a shortened key; it never needs to reach a report.
pub(crate) fn redact_stderr_line(raw: &str) -> String {
    let trimmed = raw.trim_end();
    if trimmed.trim().is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(trimmed.len().min(400));
    for token in trimmed.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        let lower = token.to_ascii_lowercase();
        let sensitive = lower.contains("sk-")
            || lower.contains("token")
            || lower.contains("secret")
            || lower.contains("password")
            || lower.contains("authorization")
            || lower.contains("bearer")
            || token.contains("://");
        out.push_str(if sensitive { "<redacted>" } else { token });
        if out.len() > 400 {
            out.push('…');
            break;
        }
    }
    out
}

/// Reads NDJSON from a child's stdout with a hard per-line bound.
pub struct LineReader {
    reader: BufReader<ChildStdout>,
    buffer: Vec<u8>,
}

impl LineReader {
    pub fn new(stdout: ChildStdout) -> Self {
        Self {
            reader: BufReader::new(stdout),
            buffer: Vec::new(),
        }
    }

    /// Returns `Ok(None)` at EOF. A line longer than `MAX_PROTOCOL_LINE_BYTES`
    /// is reported as a protocol error and the rest of that line is skipped.
    pub fn next_line(&mut self) -> Result<Option<String>, AppError> {
        loop {
            if let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<u8> = self.buffer.drain(..=index).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let text = String::from_utf8_lossy(&line).to_string();
                if text.trim().is_empty() {
                    continue;
                }
                return Ok(Some(text));
            }
            if self.buffer.len() > MAX_PROTOCOL_LINE_BYTES {
                self.buffer.clear();
                return Err(AppError::new(
                    ErrorCode::Protocol,
                    "the CLI produced a line beyond the accepted protocol bound",
                )
                .with_details(serde_json::json!({
                    "stage": "readLine",
                    "limit": MAX_PROTOCOL_LINE_BYTES,
                })));
            }
            let mut chunk = [0u8; 8192];
            let count = self.reader.read(&mut chunk).map_err(|error| {
                AppError::new(
                    ErrorCode::Io,
                    format!("failed to read from the CLI: {error}"),
                )
                .with_details(serde_json::json!({ "stage": "readLine" }))
            })?;
            if count == 0 {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let text = String::from_utf8_lossy(&self.buffer).to_string();
                self.buffer.clear();
                if text.trim().is_empty() {
                    return Ok(None);
                }
                return Ok(Some(text));
            }
            self.buffer.extend_from_slice(&chunk[..count]);
        }
    }
}

/// Absolute-path check used by the adapters before they touch a path.
pub fn is_absolute(path: &Path) -> bool {
    path.is_absolute()
}
