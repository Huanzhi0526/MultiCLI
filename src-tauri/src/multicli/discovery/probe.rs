//! Bounded, read-only version probes.
//!
//! Only an entry that has already been recognised as a trusted installation
//! may be probed. The probe:
//! - starts the resolved interpreter directly with an argv array (no shell, no
//!   `cmd /c`, no PowerShell),
//! - hides its own console window,
//! - has a hard timeout and cancels by killing **its own child only**,
//! - caps captured output at 8 KiB,
//! - never sends a prompt, never logs in, never downloads or upgrades.
//!
//! A failed probe keeps an explicit reason and never turns into "not
//! installed".

use super::signature::InstallSignature;
use super::wrapper::NodeInterpreter;
use crate::multicli::diagnostics::iso_timestamp;
use crate::multicli::store::now_ms;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(windows)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

pub const MAX_PROBE_OUTPUT: usize = 8 * 1024;
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeStatus {
    Ok,
    Failed,
    Timeout,
    Skipped,
}

impl ProbeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeOutcome {
    pub status: ProbeStatus,
    pub version: Option<String>,
    /// Path-free reason. Never contains program output, prompts or arguments.
    pub reason: Option<String>,
    pub checked_at: String,
    /// Epoch milliseconds of `checked_at`, used for cache expiry only.
    pub checked_at_ms: i64,
    pub duration_ms: u64,
    pub arguments: Vec<String>,
    pub program: String,
}

impl ProbeOutcome {
    pub fn skipped(reason: &str) -> Self {
        Self {
            status: ProbeStatus::Skipped,
            version: None,
            reason: Some(reason.to_string()),
            checked_at: iso_timestamp(now_ms()),
            checked_at_ms: now_ms(),
            duration_ms: 0,
            arguments: Vec::new(),
            program: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTarget {
    pub program: PathBuf,
    pub arguments: Vec<String>,
}

impl ProbeTarget {
    pub fn display_program(&self) -> String {
        self.program
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

/// Builds the argv for a probe. Native entries run themselves; Node package
/// entries run through the shim's interpreter with the recovered script.
pub fn build_target(
    signature: &InstallSignature,
    native: Option<&Path>,
    interpreter: Option<&NodeInterpreter>,
    entry_script: Option<&Path>,
) -> Option<ProbeTarget> {
    if let Some(native) = native {
        return Some(ProbeTarget {
            program: native.to_path_buf(),
            arguments: signature
                .version_args
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
        });
    }
    let script = entry_script?;
    let program = match interpreter {
        Some(NodeInterpreter::Resolved(path)) => path.clone(),
        _ => resolve_node_from_path()?,
    };
    let mut arguments = vec![script.to_string_lossy().to_string()];
    arguments.extend(
        signature
            .version_args
            .iter()
            .map(|value| (*value).to_string()),
    );
    Some(ProbeTarget { program, arguments })
}

/// Locates `node.exe` on the live `PATH` with a bounded, non-recursive walk.
pub fn resolve_node_from_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path).take(64) {
        let candidate = entry.join("node.exe");
        if candidate.is_file() {
            return Some(std::fs::canonicalize(&candidate).unwrap_or(candidate));
        }
    }
    None
}

/// Runs the probe. The child process belongs to this call and is the only
/// process this module is ever allowed to terminate.
pub fn probe(
    target: &ProbeTarget,
    signature: &InstallSignature,
    timeout: Duration,
) -> ProbeOutcome {
    let started = Instant::now();
    let checked_ms = now_ms();
    let checked_at = iso_timestamp(checked_ms);
    let mut command = Command::new(&target.program);
    command
        .args(&target.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: the probe must not flash a console window.
        command.creation_flags(0x0800_0000);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ProbeOutcome {
                status: ProbeStatus::Failed,
                version: None,
                reason: Some(format!("probe could not start: {}", error.kind())),
                checked_at,
                checked_at_ms: checked_ms,
                duration_ms: elapsed_ms(started),
                arguments: target.arguments.clone(),
                program: target.display_program(),
            }
        }
    };

    let stdout = child.stdout.take().map(spawn_reader);
    let stderr = child.stderr.take().map(spawn_reader);

    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    // Only this probe's own child is terminated.
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };

    let stdout_text = stdout.map(join_reader).unwrap_or_default();
    let stderr_text = stderr.map(join_reader).unwrap_or_default();
    let duration_ms = elapsed_ms(started);

    if timed_out {
        return ProbeOutcome {
            status: ProbeStatus::Timeout,
            version: None,
            reason: Some("probe exceeded its timeout and was cancelled".to_string()),
            checked_at,
            checked_at_ms: checked_ms,
            duration_ms,
            arguments: target.arguments.clone(),
            program: target.display_program(),
        };
    }
    let Some(status) = status else {
        return ProbeOutcome {
            status: ProbeStatus::Failed,
            version: None,
            reason: Some("probe process state could not be read".to_string()),
            checked_at,
            checked_at_ms: checked_ms,
            duration_ms,
            arguments: target.arguments.clone(),
            program: target.display_program(),
        };
    };
    if !status.success() {
        return ProbeOutcome {
            status: ProbeStatus::Failed,
            version: None,
            reason: Some(format!(
                "probe exited with code {}",
                status.code().unwrap_or(-1)
            )),
            checked_at,
            checked_at_ms: checked_ms,
            duration_ms,
            arguments: target.arguments.clone(),
            program: target.display_program(),
        };
    }

    let combined = format!("{stdout_text}\n{stderr_text}");
    if !signature.version_markers.is_empty()
        && !signature
            .version_markers
            .iter()
            .any(|marker| combined.contains(marker))
    {
        return ProbeOutcome {
            status: ProbeStatus::Failed,
            version: None,
            reason: Some("probe output did not contain the product marker".to_string()),
            checked_at,
            checked_at_ms: checked_ms,
            duration_ms,
            arguments: target.arguments.clone(),
            program: target.display_program(),
        };
    }
    let version = parse_version(&stdout_text).or_else(|| parse_version(&stderr_text));
    match version {
        Some(version) => ProbeOutcome {
            status: ProbeStatus::Ok,
            version: Some(version),
            reason: None,
            checked_at,
            checked_at_ms: checked_ms,
            duration_ms,
            arguments: target.arguments.clone(),
            program: target.display_program(),
        },
        None => ProbeOutcome {
            status: ProbeStatus::Failed,
            version: None,
            reason: Some("probe output contained no version number".to_string()),
            checked_at,
            checked_at_ms: checked_ms,
            duration_ms,
            arguments: target.arguments.clone(),
            program: target.display_program(),
        },
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

struct ReaderHandle {
    thread: std::thread::JoinHandle<String>,
    #[cfg(windows)]
    stop: Arc<AtomicBool>,
}

#[cfg(windows)]
fn spawn_reader(
    stream: impl Read + Send + 'static + std::os::windows::io::AsRawHandle,
) -> ReaderHandle {
    #[cfg(windows)]
    {
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        ReaderHandle {
            thread: std::thread::spawn(move || read_pipe_bounded(stream, Some(reader_stop))),
            stop,
        }
    }
}

#[cfg(not(windows))]
fn spawn_reader(stream: impl Read + Send + 'static) -> ReaderHandle {
    ReaderHandle {
        thread: std::thread::spawn(move || read_pipe_bounded(stream)),
    }
}

#[cfg(windows)]
fn read_pipe_bounded(
    mut stream: impl Read + std::os::windows::io::AsRawHandle,
    stop: Option<Arc<AtomicBool>>,
) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        #[cfg(windows)]
        {
            use windows::Win32::System::Pipes::PeekNamedPipe;
            let stop_requested = stop
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire));
            if reader_should_stop(stop_requested, buffer.len()) {
                break;
            }
            let mut available = 0u32;
            let peek = unsafe {
                PeekNamedPipe(
                    windows::Win32::Foundation::HANDLE(stream.as_raw_handle()),
                    None,
                    0,
                    None,
                    Some(&mut available),
                    None,
                )
            };
            if peek.is_err() {
                break;
            }
            if available == 0 {
                if stop_requested {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                append_limited(&mut buffer, &chunk[..count]);
            }
        }
    }
    String::from_utf8_lossy(&buffer).to_string()
}

#[cfg(not(windows))]
fn read_pipe_bounded(mut stream: impl Read) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                append_limited(&mut buffer, &chunk[..count]);
            }
        }
    }
    String::from_utf8_lossy(&buffer).to_string()
}

fn append_limited(buffer: &mut Vec<u8>, bytes: &[u8]) {
    let remaining = MAX_PROBE_OUTPUT.saturating_sub(buffer.len());
    buffer.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
}

fn reader_should_stop(stop_requested: bool, captured_len: usize) -> bool {
    stop_requested && captured_len >= MAX_PROBE_OUTPUT
}

fn join_reader(handle: ReaderHandle) -> String {
    #[cfg(windows)]
    handle.stop.store(true, Ordering::Release);
    handle.thread.join().unwrap_or_default()
}

/// Extracts the first semantic version-looking token. Product-specific
/// prefixes (`codex-cli 0.166.0`, `2.1.153 (Claude Code)`) both work.
pub fn parse_version(text: &str) -> Option<String> {
    for line in text.lines() {
        for token in line.split_whitespace() {
            let cleaned = token.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric()
                    && character != '.'
                    && character != '-'
                    && character != '+'
            });
            let without_prefix = cleaned.strip_prefix('v').unwrap_or(cleaned);
            if is_semver_like(without_prefix) {
                return Some(without_prefix.to_string());
            }
        }
    }
    None
}

fn is_semver_like(value: &str) -> bool {
    if value.is_empty() || value.len() > 64 {
        return false;
    }
    let mut parts = value.split('.');
    let major = parts.next().unwrap_or_default();
    let minor = parts.next().unwrap_or_default();
    let patch = parts.next();
    if !major.chars().all(|character| character.is_ascii_digit()) || major.is_empty() {
        return false;
    }
    if !minor.chars().all(|character| character.is_ascii_digit()) || minor.is_empty() {
        return false;
    }
    if let Some(patch) = patch {
        let core = patch.split(['-', '+']).next().unwrap_or_default();
        if !core.chars().all(|character| character.is_ascii_digit()) || core.is_empty() {
            return false;
        }
    } else {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multicli::discovery::signature::SignatureRegistry;

    #[test]
    fn version_parsing_handles_real_output_shapes() {
        assert_eq!(parse_version("codex-cli 0.166.0\n"), Some("0.166.0".into()));
        assert_eq!(
            parse_version("2.1.153 (Claude Code)"),
            Some("2.1.153".into())
        );
        assert_eq!(parse_version("v2.179.0"), Some("2.179.0".into()));
        assert_eq!(parse_version("no version here"), None);
        assert_eq!(parse_version("1.2"), None);
    }

    #[test]
    fn captured_probe_output_stays_within_the_eight_kib_limit() {
        let mut captured = Vec::new();
        append_limited(&mut captured, &vec![b'a'; MAX_PROBE_OUTPUT + 17]);
        append_limited(&mut captured, b"extra");
        assert_eq!(captured.len(), MAX_PROBE_OUTPUT);
    }

    #[test]
    fn stopped_reader_finishes_after_draining_at_most_the_capture_limit() {
        assert!(!reader_should_stop(true, MAX_PROBE_OUTPUT - 1));
        assert!(reader_should_stop(true, MAX_PROBE_OUTPUT));
        assert!(!reader_should_stop(false, MAX_PROBE_OUTPUT));
    }

    #[test]
    fn probe_runs_an_owned_child_with_a_hard_timeout() {
        let registry = SignatureRegistry::standard();
        let codex = registry.by_agent("codex").unwrap();
        let target = ProbeTarget {
            program: std::env::current_exe().unwrap(),
            arguments: vec!["--definitely-not-a-real-flag".to_string()],
        };
        // The test binary exits non-zero for an unknown flag: a failed probe is
        // reported as a failure, not as "not installed".
        let outcome = probe(&target, codex, Duration::from_secs(5));
        assert!(matches!(
            outcome.status,
            ProbeStatus::Failed | ProbeStatus::Timeout
        ));
        assert!(outcome.version.is_none());
        assert!(outcome.reason.is_some());

        let skipped = ProbeOutcome::skipped("not attempted");
        assert_eq!(skipped.status.as_str(), "skipped");
    }

    #[test]
    fn build_target_prefers_the_native_entry_and_never_uses_a_shell() {
        let registry = SignatureRegistry::standard();
        let codex = registry.by_agent("codex").unwrap();
        let target = build_target(codex, Some(Path::new(r"C:\x\codex.exe")), None, None).unwrap();
        assert_eq!(target.program, PathBuf::from(r"C:\x\codex.exe"));
        assert_eq!(target.arguments, vec!["--version".to_string()]);

        let node_target = build_target(
            codex,
            None,
            Some(&NodeInterpreter::Resolved(PathBuf::from(
                r"C:\node\node.exe",
            ))),
            Some(Path::new(r"C:\npm\node_modules\@openai\codex\bin\codex.js")),
        )
        .unwrap();
        assert_eq!(node_target.program, PathBuf::from(r"C:\node\node.exe"));
        assert_eq!(node_target.arguments.len(), 2);
        assert_eq!(node_target.arguments[1], "--version");
    }
}
