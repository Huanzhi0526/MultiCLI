//! Wire models for the managed terminal (`docs/contracts/terminal-v1.md`).
//!
//! These DTOs are additive S04 integration types. They never reuse or relax
//! the frozen S01 domain schema: a terminal Run still uses the frozen
//! `Run`/`ProcessIdentity` records, and the terminal-specific fields live here.
//!
//! Everything crossing the IPC boundary is camelCase, rejects unknown fields
//! and carries its own limit so the UI cannot ask for an unbounded buffer,
//! an unbounded read or a process it does not own.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::multicli::domain::HostPath;
use crate::multicli::error::{AppError, ErrorCode};

/// Version of the terminal contract document.
pub const TERMINAL_SCHEMA_VERSION: u32 = 1;

/// Per-terminal retained output. Small enough that eight terminals stay inside
/// the performance budget, large enough that a full-screen repaint plus a
/// scrollback burst is not truncated.
pub const DEFAULT_BUFFER_BYTES: u64 = 4 * 1024 * 1024;
pub const MIN_BUFFER_BYTES: u64 = 256 * 1024;
pub const MAX_BUFFER_BYTES: u64 = 64 * 1024 * 1024;

/// Per-terminal log file. Rotated once, then restarted.
pub const DEFAULT_LOG_BYTES: u64 = 20 * 1024 * 1024;
pub const MIN_LOG_BYTES: u64 = 1 * 1024 * 1024;
pub const MAX_LOG_BYTES: u64 = 512 * 1024 * 1024;

/// Hard cap on live terminals. Each one owns a ConPTY, a child tree, a reader
/// thread and a log file, so an unbounded tab count is a real resource leak.
pub const MAX_TERMINALS: usize = 16;

/// Upper bound for one read batch. A slow UI falls behind by *bytes*, never by
/// an unbounded queue: the reply is capped and the cursor advances.
pub const MAX_READ_BYTES: u32 = 256 * 1024;
pub const DEFAULT_READ_BYTES: u32 = 64 * 1024;

/// Upper bound for one write. A paste larger than this is rejected with a real
/// error instead of blocking the PTY writer indefinitely.
pub const MAX_WRITE_BYTES: usize = 1024 * 1024;

pub const MIN_DIMENSION: u16 = 2;
pub const MAX_DIMENSION: u16 = 1000;

pub const STREAM_NOTE: &str = "终端输出是字节流，不是结构化消息；没有证据时交互状态保持 unknown，不因静默输出判断 idle。";
pub const OWNERSHIP_NOTE: &str =
    "只停止本应用创建且 pid + creationTime 仍然一致的进程；外部同款 CLI 不在处理范围内。";

// ---------------------------------------------------------------------------
// Target selection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellFlavor {
    /// `%SystemRoot%\System32\cmd.exe`
    Cmd,
    /// Windows PowerShell 5.1 (`System32\WindowsPowerShell\v1.0\powershell.exe`)
    WindowsPowershell,
    /// PowerShell 7+ (`pwsh.exe`) — only offered when it exists on this host.
    Pwsh,
}

impl ShellFlavor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::WindowsPowershell => "windows-powershell",
            Self::Pwsh => "pwsh",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalTargetKind {
    Shell,
    Installation,
    /// A user-defined `executable + argv[] + cwd` preset (S06). It reuses the
    /// same launch chain as an installation; only its origin differs.
    #[serde(rename = "custom-cli")]
    CustomCli,
}

/// What the user picked. Exactly one of `shell` / `installationId` is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalTarget {
    pub kind: TerminalTargetKind,
    #[serde(default)]
    pub shell: Option<ShellFlavor>,
    #[serde(default)]
    pub installation_id: Option<String>,
    #[serde(default)]
    pub custom_cli_id: Option<String>,
}

impl TerminalTarget {
    pub fn validate(&self) -> Result<(), AppError> {
        match self.kind {
            TerminalTargetKind::Shell => {
                if self.installation_id.is_some() || self.custom_cli_id.is_some() {
                    return Err(AppError::invalid_argument(
                        "target.installationId",
                        "must be absent for a shell target",
                    ));
                }
                if self.shell.is_none() {
                    return Err(AppError::invalid_argument(
                        "target.shell",
                        "is required for a shell target",
                    ));
                }
            }
            TerminalTargetKind::Installation => {
                if self.shell.is_some() {
                    return Err(AppError::invalid_argument(
                        "target.shell",
                        "must be absent for an installation target",
                    ));
                }
                if self.custom_cli_id.is_some() {
                    return Err(AppError::invalid_argument(
                        "target.customCliId",
                        "must be absent for an installation target",
                    ));
                }
                match self.installation_id.as_deref() {
                    Some(id) if !id.trim().is_empty() => {}
                    _ => {
                        return Err(AppError::invalid_argument(
                            "target.installationId",
                            "is required for an installation target",
                        ))
                    }
                }
            }
            TerminalTargetKind::CustomCli => {
                if self.shell.is_some() {
                    return Err(AppError::invalid_argument(
                        "target.shell",
                        "must be absent for a custom CLI target",
                    ));
                }
                if self.installation_id.is_some() {
                    return Err(AppError::invalid_argument(
                        "target.installationId",
                        "must be absent for a custom CLI target",
                    ));
                }
                match self.custom_cli_id.as_deref() {
                    Some(id) if !id.trim().is_empty() => {}
                    _ => {
                        return Err(AppError::invalid_argument(
                            "target.customCliId",
                            "is required for a custom CLI target",
                        ))
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaunchChain {
    /// Direct `CreateProcess` of a native image.
    Exe,
    /// `cmd.exe /d /s /v:off /c %MULTICLI_CMD_FILE%`; the trampoline holds the
    /// fully escaped command, so no user text reaches cmd's own parser.
    Cmd,
    /// `pwsh.exe (7.3+) ... -File <trampoline.ps1>`; the trampoline holds
    /// single-quoted literals, so no user text reaches PowerShell's parser.
    Powershell,
}

impl LaunchChain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exe => "exe",
            Self::Cmd => "cmd",
            Self::Powershell => "powershell",
        }
    }

    /// Chain selected from the pinned executable's extension. An unknown
    /// extension is `Unsupported` rather than a guess.
    pub fn for_extension(path: &Path) -> Option<Self> {
        match path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref()
        {
            Some("exe") => Some(Self::Exe),
            Some("cmd") | Some("bat") => Some(Self::Cmd),
            Some("ps1") => Some(Self::Powershell),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

impl EnvVar {
    fn validate(&self, index: usize) -> Result<(), AppError> {
        if self.name.is_empty() || self.name.len() > 128 {
            return Err(AppError::invalid_argument(
                &format!("env[{index}].name"),
                "must be 1..=128 characters",
            ));
        }
        if self
            .name
            .chars()
            .any(|c| c == '=' || c.is_control() || c == '\0')
        {
            return Err(AppError::invalid_argument(
                &format!("env[{index}].name"),
                "must not contain '=', control characters or NUL",
            ));
        }
        if self.value.contains('\0') {
            return Err(AppError::invalid_argument(
                &format!("env[{index}].value"),
                "must not contain NUL",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalOpenSpec {
    pub target: TerminalTarget,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: String,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub title: Option<String>,
}

impl TerminalOpenSpec {
    pub fn validate(&self) -> Result<(), AppError> {
        self.target.validate()?;
        if self.args.len() > 256 {
            return Err(AppError::invalid_argument(
                "args",
                "at most 256 arguments are accepted",
            ));
        }
        if self.target.kind == TerminalTargetKind::Shell && !self.args.is_empty() {
            // A shell target is started with a fixed launcher argv; accepting
            // arguments would make them part of a shell command line, which is
            // exactly what this contract forbids. Rejected before any Run is
            // created.
            return Err(AppError::invalid_argument(
                "args",
                "an interactive shell target does not accept arguments",
            ));
        }
        for (index, arg) in self.args.iter().enumerate() {
            if arg.contains('\0') {
                return Err(AppError::invalid_argument(
                    &format!("args[{index}]"),
                    "must not contain NUL",
                ));
            }
            if arg.len() > 32 * 1024 {
                return Err(AppError::invalid_argument(
                    &format!("args[{index}]"),
                    "must be at most 32 KiB",
                ));
            }
        }
        if self.cwd.trim().is_empty() {
            return Err(AppError::invalid_argument("cwd", "must not be empty"));
        }
        if !Path::new(&self.cwd).is_absolute() {
            return Err(AppError::invalid_argument("cwd", "must be an absolute path"));
        }
        if !Path::new(&self.cwd).is_dir() {
            return Err(AppError::invalid_argument(
                "cwd",
                "must be an existing directory",
            ));
        }
        if self.env.len() > 64 {
            return Err(AppError::invalid_argument(
                "env",
                "at most 64 environment variables are accepted",
            ));
        }
        for (index, item) in self.env.iter().enumerate() {
            item.validate(index)?;
        }
        let mut seen = std::collections::BTreeSet::new();
        for item in &self.env {
            if !seen.insert(item.name.to_ascii_lowercase()) {
                return Err(AppError::invalid_argument(
                    "env",
                    "environment variable names must be unique (case-insensitive)",
                ));
            }
        }
        self.validate_size()?;
        if let Some(title) = &self.title {
            if title.len() > 200 {
                return Err(AppError::invalid_argument(
                    "title",
                    "must be at most 200 characters",
                ));
            }
        }
        Ok(())
    }

    pub fn validate_size(&self) -> Result<(), AppError> {
        for (field, value) in [("cols", self.cols), ("rows", self.rows)] {
            if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&value) {
                return Err(AppError::invalid_argument(
                    field,
                    format!("must be between {MIN_DIMENSION} and {MAX_DIMENSION}"),
                ));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Lifecycle view
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalState {
    Starting,
    Running,
    Stopping,
    Exited,
    Failed,
}

impl TerminalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Exited => "exited",
            Self::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Exited | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalStopMode {
    /// Ctrl+C written to the PTY: interrupts the foreground job, keeps the
    /// shell/CLI alive. Never terminates the process.
    Interrupt,
    /// Ctrl+C, bounded wait, then the owned tree. Reports escalation.
    Gentle,
    /// Ends the owned tree immediately.
    Force,
}

impl TerminalStopMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::Gentle => "gentle",
            Self::Force => "force",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobGuardMode {
    /// Job assigned and configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
    KillOnClose,
    /// Job assignment failed; the terminal was refused rather than run without
    /// a tree constraint. Kept in the view for an honest failure record.
    Unassigned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobGuardView {
    pub mode: JobGuardMode,
    pub assigned: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalBuffersView {
    pub buffer_capacity_bytes: u64,
    pub buffered_bytes: u64,
    pub dropped_bytes: u64,
    pub log_path: String,
    pub log_capacity_bytes: u64,
    pub log_bytes: u64,
    pub log_rotations: u64,
    pub log_dropped_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalExitView {
    pub code: Option<i32>,
    pub success: bool,
    pub ended_at: String,
    /// `true` only when the OS reported an exit status; a lost reader is not an
    /// exit and is reported as a failure instead.
    pub reported: bool,
    pub signal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalStopReport {
    pub requested: TerminalStopMode,
    pub interrupt_sent: bool,
    pub escalated: bool,
    pub waited_ms: u64,
    pub already_terminal: bool,
    pub refused_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalView {
    pub schema_version: u32,
    pub terminal_id: String,
    pub run_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub installation_id: Option<String>,
    pub title: String,
    pub target_kind: TerminalTargetKind,
    pub product: Option<String>,
    pub executable: HostPath,
    pub launch_chain: LaunchChain,
    /// Exit code of the launcher kind, shown to the user: the exact argv the
    /// application intends to spawn. Never a user-typed shell string.
    pub launch_argv: Vec<String>,
    pub args: Vec<String>,
    pub cwd: HostPath,
    /// Environment variable *names* only. Values are not echoed back.
    pub env_names: Vec<String>,
    pub state: TerminalState,
    pub interaction_status: String,
    pub identity: Option<crate::multicli::domain::ProcessIdentity>,
    pub job: JobGuardView,
    pub buffers: TerminalBuffersView,
    pub exit: Option<TerminalExitView>,
    pub stop: Option<TerminalStopReport>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub evidence: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalListView {
    pub schema_version: u32,
    pub items: Vec<TerminalView>,
    pub limits: TerminalLimits,
    pub active_count: u32,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalReadRequest {
    pub terminal_id: String,
    /// Absolute stream offset already delivered. `null` starts at the oldest
    /// byte still retained.
    #[serde(default)]
    pub cursor: Option<u64>,
    #[serde(default)]
    pub max_bytes: Option<u32>,
}

impl TerminalReadRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.terminal_id.trim().is_empty() {
            return Err(AppError::invalid_argument(
                "terminalId",
                "must not be empty",
            ));
        }
        if let Some(max) = self.max_bytes {
            if max == 0 || max > MAX_READ_BYTES {
                return Err(AppError::invalid_argument(
                    "maxBytes",
                    format!("must be between 1 and {MAX_READ_BYTES}"),
                ));
            }
        }
        Ok(())
    }

    pub fn limit(&self) -> usize {
        self.max_bytes.unwrap_or(DEFAULT_READ_BYTES) as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalReadView {
    pub terminal_id: String,
    pub run_id: String,
    pub stream_id: String,
    /// Stream offset of the first delivered byte.
    pub from_seq: u64,
    /// Stream offset the next read must pass as `cursor`.
    pub to_seq: u64,
    /// UTF-8 text. A multi-byte character or an escape sequence that is still
    /// incomplete is held back and arrives with the next read.
    pub text: String,
    pub bytes: u64,
    /// Bytes discarded from the front of the stream before this read.
    pub dropped_before: u64,
    pub dropped_bytes: u64,
    pub history_truncated: bool,
    pub buffered_bytes: u64,
    pub state: TerminalState,
    pub exit: Option<TerminalExitView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalWriteRequest {
    pub terminal_id: String,
    pub data: String,
}

impl TerminalWriteRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.terminal_id.trim().is_empty() {
            return Err(AppError::invalid_argument(
                "terminalId",
                "must not be empty",
            ));
        }
        if self.data.is_empty() {
            return Err(AppError::invalid_argument("data", "must not be empty"));
        }
        if self.data.len() > MAX_WRITE_BYTES {
            return Err(AppError::invalid_argument(
                "data",
                format!("must be at most {MAX_WRITE_BYTES} bytes per write"),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalWriteView {
    pub terminal_id: String,
    pub accepted_bytes: u64,
    pub state: TerminalState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalResizeRequest {
    pub terminal_id: String,
    pub cols: u16,
    pub rows: u16,
}

impl TerminalResizeRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.terminal_id.trim().is_empty() {
            return Err(AppError::invalid_argument(
                "terminalId",
                "must not be empty",
            ));
        }
        for (field, value) in [("cols", self.cols), ("rows", self.rows)] {
            if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&value) {
                return Err(AppError::invalid_argument(
                    field,
                    format!("must be between {MIN_DIMENSION} and {MAX_DIMENSION}"),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalResizeView {
    pub terminal_id: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalStopRequest {
    pub terminal_id: String,
    pub mode: TerminalStopMode,
    #[serde(default)]
    pub grace_ms: Option<u32>,
}

impl TerminalStopRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.terminal_id.trim().is_empty() {
            return Err(AppError::invalid_argument(
                "terminalId",
                "must not be empty",
            ));
        }
        if let Some(grace) = self.grace_ms {
            if !(100..=60_000).contains(&grace) {
                return Err(AppError::invalid_argument(
                    "graceMs",
                    "must be between 100 and 60000",
                ));
            }
        }
        Ok(())
    }

    pub fn grace(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.grace_ms.unwrap_or(3_000) as u64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalStopView {
    pub terminal: TerminalView,
    pub stop: TerminalStopReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalCloseRequest {
    pub terminal_id: String,
    #[serde(default)]
    pub mode: Option<TerminalStopMode>,
    #[serde(default)]
    pub grace_ms: Option<u32>,
}

impl TerminalCloseRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.terminal_id.trim().is_empty() {
            return Err(AppError::invalid_argument(
                "terminalId",
                "must not be empty",
            ));
        }
        if let Some(grace) = self.grace_ms {
            if !(100..=60_000).contains(&grace) {
                return Err(AppError::invalid_argument(
                    "graceMs",
                    "must be between 100 and 60000",
                ));
            }
        }
        Ok(())
    }

    pub fn mode(&self) -> TerminalStopMode {
        self.mode.unwrap_or(TerminalStopMode::Gentle)
    }

    pub fn grace(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.grace_ms.unwrap_or(3_000) as u64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalCloseView {
    pub terminal_id: String,
    pub removed: bool,
    pub stop: TerminalStopReport,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalLimits {
    pub buffer_bytes: u64,
    pub min_buffer_bytes: u64,
    pub max_buffer_bytes: u64,
    pub log_bytes: u64,
    pub min_log_bytes: u64,
    pub max_log_bytes: u64,
    pub max_terminals: u32,
    pub note: String,
}

impl Default for TerminalLimits {
    fn default() -> Self {
        Self {
            buffer_bytes: DEFAULT_BUFFER_BYTES,
            min_buffer_bytes: MIN_BUFFER_BYTES,
            max_buffer_bytes: MAX_BUFFER_BYTES,
            log_bytes: DEFAULT_LOG_BYTES,
            min_log_bytes: MIN_LOG_BYTES,
            max_log_bytes: MAX_LOG_BYTES,
            max_terminals: MAX_TERMINALS as u32,
            note: "缓冲与日志上限按终端独立计算；只在 MultiCLI 自己的 profile 目录内写日志。".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalLimitsUpdate {
    #[serde(default)]
    pub buffer_bytes: Option<u64>,
    #[serde(default)]
    pub log_bytes: Option<u64>,
}

impl TerminalLimitsUpdate {
    pub fn validate(&self) -> Result<(), AppError> {
        if let Some(value) = self.buffer_bytes {
            if !(MIN_BUFFER_BYTES..=MAX_BUFFER_BYTES).contains(&value) {
                return Err(AppError::invalid_argument(
                    "bufferBytes",
                    format!("must be between {MIN_BUFFER_BYTES} and {MAX_BUFFER_BYTES}"),
                ));
            }
        }
        if let Some(value) = self.log_bytes {
            if !(MIN_LOG_BYTES..=MAX_LOG_BYTES).contains(&value) {
                return Err(AppError::invalid_argument(
                    "logBytes",
                    format!("must be between {MIN_LOG_BYTES} and {MAX_LOG_BYTES}"),
                ));
            }
        }
        Ok(())
    }

    pub fn apply(&self, limits: &mut TerminalLimits) {
        if let Some(value) = self.buffer_bytes {
            limits.buffer_bytes = value;
        }
        if let Some(value) = self.log_bytes {
            limits.log_bytes = value;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalTargetView {
    /// Stable selector: `shell:cmd`, `shell:windows-powershell`, `shell:pwsh`
    /// or `installation:<id>`.
    pub id: String,
    pub kind: TerminalTargetKind,
    pub label: String,
    pub agent_id: Option<String>,
    pub product: Option<String>,
    pub installation_id: Option<String>,
    /// Set only for a custom CLI target (`kind = custom-cli`).
    pub custom_cli_id: Option<String>,
    pub executable: Option<HostPath>,
    pub launch_chain: Option<LaunchChain>,
    /// `ready` | `missing` | `unsupported` | `unverified`
    pub status: String,
    pub reason: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalTargetsView {
    pub schema_version: u32,
    pub items: Vec<TerminalTargetView>,
    pub limits: TerminalLimits,
    pub cwd_default: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalShutdownAllRequest {
    #[serde(default)]
    pub mode: Option<TerminalStopMode>,
    #[serde(default)]
    pub grace_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalShutdownAllView {
    pub stopped: Vec<TerminalCloseView>,
    pub remaining: u32,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalQuitCheckView {
    pub active_count: u32,
    pub needs_confirmation: bool,
    pub note: String,
}

/// Event emitted to the frontend. A notification only: the payload is a few
/// bytes and the actual output is pulled with `multicli_terminal_read`, so a
/// slow WebView cannot build an unbounded IPC queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalNotify {
    pub kind: String,
    pub terminal_id: String,
    pub run_id: String,
    pub stream_id: String,
    pub seq: u64,
    pub state: TerminalState,
    pub buffered_bytes: u64,
    pub dropped_bytes: u64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn host_path(host_id: &str, path: &Path) -> HostPath {
    let display = canonical_display(path);
    HostPath {
        host_id: host_id.to_string(),
        display_path: display.display().to_string(),
        canonical_path: canonical_identity(path),
    }
}

/// `dunce::canonicalize` strips the `\\?\` verbatim prefix that `std` adds on
/// Windows, so the stored path is the one a user recognises.
pub fn canonical_display(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn canonical_identity(path: &Path) -> Option<String> {
    dunce::canonicalize(path)
        .ok()
        .map(|value| value.display().to_string())
}

/// Filesystem identity of a pinned executable: canonical path, byte length and
/// last-write time. Cheaper than hashing a 100 MB image, and enough to prove
/// the file did not change between resolution and spawn.
pub fn executable_fingerprint(path: &Path) -> Result<String, AppError> {
    let meta = std::fs::metadata(path).map_err(|error| {
        AppError::new(
            ErrorCode::Unsupported,
            format!("executable is not readable: {error}"),
        )
        .with_details(serde_json::json!({ "stage": "executableFingerprint" }))
    })?;
    if !meta.is_file() {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "selected target is not a regular file",
        )
        .with_details(serde_json::json!({ "stage": "executableFingerprint" })));
    }
    let modified = meta
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_millis())
        .unwrap_or(0);
    Ok(format!(
        "{}|{}|{}",
        canonical_identity(path).unwrap_or_else(|| path.display().to_string()),
        meta.len(),
        modified
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_validation_rejects_mixed_and_missing_selectors() {
        let mixed = TerminalTarget {
            kind: TerminalTargetKind::Shell,
            shell: Some(ShellFlavor::Cmd),
            installation_id: Some("i-1".into()),
            custom_cli_id: None,
        };
        assert!(mixed.validate().is_err());

        let missing_shell = TerminalTarget {
            kind: TerminalTargetKind::Shell,
            shell: None,
            installation_id: None,
            custom_cli_id: None,
        };
        assert!(missing_shell.validate().is_err());

        let installation_without_id = TerminalTarget {
            kind: TerminalTargetKind::Installation,
            shell: None,
            installation_id: Some("  ".into()),
            custom_cli_id: None,
        };
        assert!(installation_without_id.validate().is_err());

        let ok = TerminalTarget {
            kind: TerminalTargetKind::Installation,
            shell: None,
            installation_id: Some("i-1".into()),
            custom_cli_id: None,
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn open_spec_validation_is_structured_and_bounded() {
        let cwd = std::env::temp_dir().display().to_string();
        let mut spec = TerminalOpenSpec {
            target: TerminalTarget {
                kind: TerminalTargetKind::Shell,
                shell: Some(ShellFlavor::Cmd),
                installation_id: None,
                custom_cli_id: None,
            },
            args: Vec::new(),
            cwd: cwd.clone(),
            env: vec![EnvVar {
                name: "MULTICLI_S04".into(),
                value: "1".into(),
            }],
            cols: 100,
            rows: 30,
            title: None,
        };
        assert!(spec.validate().is_ok());

        spec.cols = 0;
        assert!(spec.validate().is_err());
        spec.cols = 100;
        spec.rows = MAX_DIMENSION + 1;
        assert!(spec.validate().is_err());
        spec.rows = 30;

        spec.cwd = "relative".into();
        assert!(spec.validate().is_err());
        spec.cwd = cwd;

        spec.env = vec![
            EnvVar {
                name: "A".into(),
                value: "1".into(),
            },
            EnvVar {
                name: "a".into(),
                value: "2".into(),
            },
        ];
        assert!(spec.validate().is_err());

        spec.env = vec![EnvVar {
            name: "BAD=NAME".into(),
            value: "1".into(),
        }];
        assert!(spec.validate().is_err());

        spec.env.clear();
        spec.args = vec!["a\0b".into()];
        assert!(spec.validate().is_err());
        spec.args = vec!["typed".into()];
        // A shell target can never carry arguments: they would become part of
        // a shell command line.
        assert!(spec.validate().is_err());
        spec.target.kind = TerminalTargetKind::Installation;
        spec.target.shell = None;
        spec.target.installation_id = Some("inst-1".into());
        assert!(spec.validate().is_ok());
        spec.args.clear();
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn launch_chain_is_derived_from_extension_only() {
        assert_eq!(
            LaunchChain::for_extension(Path::new(r"C:\x\a.EXE")),
            Some(LaunchChain::Exe)
        );
        assert_eq!(
            LaunchChain::for_extension(Path::new(r"C:\x\a.cmd")),
            Some(LaunchChain::Cmd)
        );
        assert_eq!(
            LaunchChain::for_extension(Path::new(r"C:\x\a.bat")),
            Some(LaunchChain::Cmd)
        );
        assert_eq!(
            LaunchChain::for_extension(Path::new(r"C:\x\a.ps1")),
            Some(LaunchChain::Powershell)
        );
        assert_eq!(LaunchChain::for_extension(Path::new(r"C:\x\codex")), None);
    }

    #[test]
    fn limits_update_respects_its_own_bounds() {
        let mut limits = TerminalLimits::default();
        let ok = TerminalLimitsUpdate {
            buffer_bytes: Some(MIN_BUFFER_BYTES),
            log_bytes: Some(MAX_LOG_BYTES),
        };
        assert!(ok.validate().is_ok());
        ok.apply(&mut limits);
        assert_eq!(limits.buffer_bytes, MIN_BUFFER_BYTES);

        let too_small = TerminalLimitsUpdate {
            buffer_bytes: Some(1),
            log_bytes: None,
        };
        assert!(too_small.validate().is_err());
        let too_big = TerminalLimitsUpdate {
            buffer_bytes: None,
            log_bytes: Some(MAX_LOG_BYTES + 1),
        };
        assert!(too_big.validate().is_err());
    }
}
