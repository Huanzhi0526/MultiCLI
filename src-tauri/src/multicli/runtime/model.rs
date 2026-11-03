//! Wire models for the S05 structured runtime (`docs/contracts/runtime-v1.md`).
//!
//! These are additive integration DTOs. The frozen S01 `Run`/`Session`/
//! `ProcessIdentity` records stay untouched: a runtime still creates one frozen
//! `Run` per protocol connection, and everything a protocol needs to say about
//! itself lives here.
//!
//! Two rules shape every type below:
//!
//! 1. A native tool-call id and a permission-request id are **different
//!    things** and never collapse into one string. A tool call can be denied
//!    without its own permission record existing (for example a CLI that asks
//!    once for a whole batch), and one permission request can cover a command
//!    that produces two tool calls.
//! 2. Nothing is inferred from silence. An interaction state is `unknown`
//!    unless a protocol message proved otherwise.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

use crate::multicli::domain::HostPath;
use crate::multicli::error::AppError;

/// Version of the runtime contract document.
pub const RUNTIME_SCHEMA_VERSION: u32 = 1;

/// Upper bound for one `runtime_send` text. A prompt larger than this belongs
/// in a file the CLI reads, not in one IPC frame.
pub const MAX_PROMPT_BYTES: usize = 256 * 1024;

/// Upper bound for a follow-up message.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Hard cap on simultaneously open protocol connections. Each one owns a child
/// process, a reader thread and a bounded journal.
pub const MAX_RUNS: usize = 8;

/// Journal retention per run: the streaming tail the UI can still replay after
/// a reconnect. Durable messages are persisted separately, so this is a display
/// buffer, not the history.
pub const JOURNAL_MAX_EVENTS: usize = 4000;
pub const JOURNAL_MAX_BYTES: usize = 4 * 1024 * 1024;

/// One `multicli_runtime_read` batch.
pub const MAX_EVENT_BATCH: u32 = 512;
pub const DEFAULT_EVENT_BATCH: u32 = 256;

/// Longest single protocol line accepted from a CLI. A line beyond this is a
/// real protocol failure, not something to buffer forever.
pub const MAX_PROTOCOL_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Default time a permission request stays answerable. When it elapses the
/// request is answered with the protocol's own deny option and recorded as
/// `expired`; nothing is ever auto-allowed.
pub const DEFAULT_PERMISSION_TIMEOUT_MS: u64 = 300_000;
pub const MIN_PERMISSION_TIMEOUT_MS: u64 = 5_000;
pub const MAX_PERMISSION_TIMEOUT_MS: u64 = 3_600_000;

pub const RUNTIME_NOTE: &str = "结构化消息来自 CLI 自己的协议；本工作台不解析终端字节冒充消息，也不在没有证据时断言交互状态。";
pub const PERMISSION_NOTE: &str =
    "权限请求默认不回答；只有用户点选的实际选项会被发送，且只对同一个 Run 的同一个请求生效。";

// ---------------------------------------------------------------------------
// Products
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeProduct {
    Codex,
    ClaudeCode,
    CodeBuddy,
}

impl RuntimeProduct {
    pub const ALL: [RuntimeProduct; 3] = [
        RuntimeProduct::Codex,
        RuntimeProduct::ClaudeCode,
        RuntimeProduct::CodeBuddy,
    ];

    pub fn agent_id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
            Self::CodeBuddy => "codebuddy",
        }
    }

    /// Stable key shared with `history::model::PRODUCTS`.
    pub fn product_key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::CodeBuddy => "codebuddy-code",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex CLI",
            Self::ClaudeCode => "Claude Code",
            Self::CodeBuddy => "CodeBuddy Code",
        }
    }

    pub fn transport(self) -> &'static str {
        match self {
            // `codex app-server` is Codex's own JSON-RPC surface.
            Self::Codex => "native",
            // Claude Code's documented structured CLI (print + stream-json).
            Self::ClaudeCode => "native",
            // CodeBuddy speaks Agent Client Protocol over stdio.
            Self::CodeBuddy => "acp",
        }
    }

    pub fn for_agent(agent_id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|product| product.agent_id() == agent_id)
    }
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

fn require_text(field: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() {
        return Err(AppError::invalid_argument(field, "must not be empty"));
    }
    Ok(())
}

/// Opens a protocol connection. `prompt` is the *user's* first message: a
/// connection alone never sends anything to a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeStartRequest {
    pub installation_id: String,
    pub cwd: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub permission_timeout_ms: Option<u64>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

impl RuntimeStartRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("installationId", &self.installation_id)?;
        validate_cwd("cwd", &self.cwd)?;
        if let Some(prompt) = &self.prompt {
            validate_text("prompt", prompt, MAX_PROMPT_BYTES)?;
        }
        if let Some(title) = &self.title {
            if title.len() > 200 {
                return Err(AppError::invalid_argument(
                    "title",
                    "must be at most 200 characters",
                ));
            }
        }
        validate_timeout(self.permission_timeout_ms)?;
        if let Some(model) = &self.model {
            require_text("model", model)?;
        }
        if let Some(effort) = &self.effort {
            require_text("effort", effort)?;
        }
        Ok(())
    }

    pub fn permission_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(
            self.permission_timeout_ms
                .unwrap_or(DEFAULT_PERMISSION_TIMEOUT_MS),
        )
    }
}

/// Continues a historical session: a **new** Run on the **same** Session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeResumeRequest {
    pub session_id: String,
    #[serde(default)]
    pub installation_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub permission_timeout_ms: Option<u64>,
}

impl RuntimeResumeRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("sessionId", &self.session_id)?;
        if let Some(cwd) = &self.cwd {
            validate_cwd("cwd", cwd)?;
        }
        if let Some(prompt) = &self.prompt {
            validate_text("prompt", prompt, MAX_PROMPT_BYTES)?;
        }
        validate_timeout(self.permission_timeout_ms)?;
        if let Some(id) = &self.installation_id {
            require_text("installationId", id)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeSendRequest {
    pub run_id: String,
    pub text: String,
}

impl RuntimeSendRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("runId", &self.run_id)?;
        validate_text("text", &self.text, MAX_MESSAGE_BYTES)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeCancelRequest {
    pub run_id: String,
    #[serde(default)]
    pub grace_ms: Option<u32>,
}

impl RuntimeCancelRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("runId", &self.run_id)?;
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
        std::time::Duration::from_millis(self.grace_ms.unwrap_or(10_000) as u64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeStopRequest {
    pub run_id: String,
    #[serde(default)]
    pub grace_ms: Option<u32>,
}

impl RuntimeStopRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("runId", &self.run_id)?;
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

/// Answers a permission request. `option_id` must be one of the options the
/// request actually carried, compared literally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeAnswerRequest {
    pub run_id: String,
    pub permission_id: String,
    pub option_id: String,
}

impl RuntimeAnswerRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("runId", &self.run_id)?;
        require_text("permissionId", &self.permission_id)?;
        require_text("optionId", &self.option_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeReadRequest {
    pub run_id: String,
    #[serde(default)]
    pub cursor: Option<u64>,
    #[serde(default)]
    pub max_events: Option<u32>,
}

impl RuntimeReadRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("runId", &self.run_id)?;
        if let Some(max) = self.max_events {
            if max == 0 || max > MAX_EVENT_BATCH {
                return Err(AppError::invalid_argument(
                    "maxEvents",
                    format!("must be between 1 and {MAX_EVENT_BATCH}"),
                ));
            }
        }
        Ok(())
    }

    pub fn limit(&self) -> usize {
        self.max_events.unwrap_or(DEFAULT_EVENT_BATCH) as usize
    }
}

fn validate_text(field: &str, value: &str, limit: usize) -> Result<(), AppError> {
    require_text(field, value)?;
    if value.len() > limit {
        return Err(AppError::invalid_argument(
            field,
            format!("must be at most {limit} bytes"),
        ));
    }
    if value.contains('\0') {
        return Err(AppError::invalid_argument(field, "must not contain NUL"));
    }
    Ok(())
}

fn validate_cwd(field: &str, value: &str) -> Result<(), AppError> {
    require_text(field, value)?;
    if !Path::new(value).is_absolute() {
        return Err(AppError::invalid_argument(field, "must be an absolute path"));
    }
    if !Path::new(value).is_dir() {
        return Err(AppError::invalid_argument(
            field,
            "must be an existing directory",
        ));
    }
    Ok(())
}

fn validate_timeout(value: Option<u64>) -> Result<(), AppError> {
    if let Some(ms) = value {
        if !(MIN_PERMISSION_TIMEOUT_MS..=MAX_PERMISSION_TIMEOUT_MS).contains(&ms) {
            return Err(AppError::invalid_argument(
                "permissionTimeoutMs",
                format!(
                    "must be between {MIN_PERMISSION_TIMEOUT_MS} and {MAX_PERMISSION_TIMEOUT_MS}"
                ),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimePermissionOption {
    /// The literal id the protocol offered. Echoing anything else is refused
    /// before it reaches the CLI.
    pub option_id: String,
    pub label: String,
    /// `allow` | `allow-always` | `deny` | `deny-always` | `cancel`
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimePermissionView {
    pub permission_id: String,
    /// The CLI's own request id, kept verbatim so a report can be matched
    /// against a protocol log.
    pub native_request_id: Option<String>,
    /// The tool call this request is about, when the protocol said so.
    pub tool_call_id: Option<String>,
    pub title: String,
    pub detail: Option<String>,
    /// `command` | `file-write` | `tool` | `other`
    pub kind: String,
    pub options: Vec<RuntimePermissionOption>,
    /// `pending` | `answered` | `expired` | `cancelled`
    pub state: String,
    pub selected_option_id: Option<String>,
    pub auto_denied: bool,
    pub reason: Option<String>,
    pub turn_id: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeMessageView {
    /// Stable within the Run: the protocol's own message id when it has one.
    pub message_id: String,
    /// `assistant` | `user` | `system`
    pub role: String,
    pub text: String,
    pub complete: bool,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeToolView {
    pub tool_call_id: String,
    /// `permission_id` is deliberately separate: one request can cover a
    /// command that yields several tool calls and vice versa.
    pub permission_id: Option<String>,
    /// **Authorization ledger.** What the user decided, or that nobody was
    /// asked. Not every tool call raises a permission request — Claude Code
    /// runs reads and project-local edits silently in its default mode — so
    /// "no request" must be its own value and never read as "denied".
    ///
    /// `not-requested` | `pending` | `allowed` | `allowed-always` | `denied` |
    /// `auto-denied`
    pub authorization: String,
    /// **Execution ledger.** Whether the tool ran at all.
    ///
    /// `unknown` | `running` | `executed` | `not-executed`
    pub execution: String,
    /// **Result ledger.** Whether an executed call succeeded. Separate from
    /// `execution` because `executed` does not imply `ok`: a read of a missing
    /// file runs and fails, and a call can be allowed and then never report.
    ///
    /// `unknown` | `ok` | `failed`
    pub outcome: String,
    pub name: String,
    /// `command` | `file-change` | `search` | `web` | `mcp` | `other`
    pub kind: String,
    /// `pending` | `running` | `completed` | `failed` | `denied` | `cancelled`
    pub status: String,
    pub input: Option<Value>,
    pub output: Option<String>,
    /// The protocol's own error flag, echoed verbatim. **Never used as a
    /// judgement.**
    ///
    /// Measured on Claude Code 2.1.153 over every captured `tool_result`
    /// (n=15): on success the field is deterministically different per tool
    /// (`Write` omits it 3/3, `Bash` sets `false` 1/1), and on failure the same
    /// `true` covers five unrelated causes — a user denial, a client contract
    /// error, an auto-deny, a hook denial, and a plain execution failure
    /// ("File does not exist", "Exit code 2"). Treating it as a state would
    /// merge all five.
    ///
    /// `execution` + `outcome` carry the meaning; this field stays so a report
    /// can quote what the CLI actually said.
    pub is_error: bool,
    pub started_at: String,
    pub ended_at: Option<String>,
}

impl RuntimeToolView {
    /// The four quadrants a report needs, derived rather than guessed:
    /// silence (no request) is never folded into denial.
    pub fn quadrant(&self) -> &'static str {
        let allowed = matches!(self.authorization.as_str(), "allowed" | "allowed-always");
        let denied = matches!(self.authorization.as_str(), "denied" | "auto-denied");
        let asked = allowed || denied || self.authorization == "pending";
        match (asked, denied, self.execution.as_str(), self.outcome.as_str()) {
            (true, false, "executed", "ok") => "allowed-executed-ok",
            (true, false, "executed", _) => "allowed-executed-failed",
            (true, false, "not-executed", _) => "allowed-not-executed",
            (_, true, _, _) => "denied-not-executed",
            (false, _, "executed", "ok") => "never-asked-executed-ok",
            (false, _, "executed", _) => "never-asked-executed-failed",
            (false, _, "not-executed", _) => "never-asked-not-executed",
            _ => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeTurnView {
    pub turn_id: String,
    pub state: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeRunView {
    pub schema_version: u32,
    pub run_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub installation_id: Option<String>,
    pub product: String,
    pub product_name: String,
    /// `native` | `acp`
    pub transport: String,
    pub title: String,
    pub cwd: HostPath,
    /// Frozen `Run.executionStatus` spelling.
    pub execution_state: String,
    /// `working` | `awaiting-permission` | `awaiting-input` | `idle` | `unknown`
    pub interaction_state: String,
    pub turn: Option<RuntimeTurnView>,
    pub native_session_id: Option<String>,
    pub process: Option<crate::multicli::domain::ProcessIdentity>,
    pub launch_argv: Vec<String>,
    pub messages: Vec<RuntimeMessageView>,
    pub tools: Vec<RuntimeToolView>,
    pub permissions: Vec<RuntimePermissionView>,
    /// Highest event seq produced so far; the next read starts after it.
    pub last_seq: u64,
    /// `true` when the journal dropped older events: the UI must resync from
    /// this view instead of assuming it has a contiguous stream.
    pub journal_truncated: bool,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// `true` only when this Run's native session id is known, so "continue"
    /// would address the same conversation.
    pub resumable: bool,
    pub notes: Vec<String>,
    pub error: Option<AppError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeEventPage {
    pub schema_version: u32,
    pub run_id: String,
    pub session_id: String,
    pub items: Vec<crate::multicli::domain::EventEnvelope>,
    pub next_cursor: u64,
    /// Events dropped before this page. A non-zero value means the client must
    /// resync from `multicli_runtime_get`.
    pub dropped_before: u64,
    pub last_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeListView {
    pub schema_version: u32,
    pub items: Vec<RuntimeRunView>,
    pub active_count: u32,
    pub max_runs: u32,
    pub note: String,
}

/// A capability row. `status` and `evidence` are separate on purpose: a
/// documented method is not a live one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeCapabilityView {
    pub product: String,
    pub product_name: String,
    pub agent_id: String,
    pub installation_id: Option<String>,
    pub version: Option<String>,
    pub executable: Option<HostPath>,
    /// `native` | `acp`
    pub transport: String,
    /// `supported` | `unsupported` | `unverified`
    pub structured_chat: String,
    pub permission_flow: String,
    pub tool_events: String,
    pub cancel: String,
    pub resume: String,
    /// `live` | `document` | `fixture`
    pub evidence: String,
    pub entry: Option<String>,
    pub reason: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeCapabilitiesView {
    pub schema_version: u32,
    pub items: Vec<RuntimeCapabilityView>,
    pub note: String,
}

/// Small notification pushed to the WebView. The payload is a few dozen bytes;
/// the actual events are pulled with `multicli_runtime_read`, so a slow WebView
/// can never build an unbounded IPC queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeNotify {
    pub run_id: String,
    pub seq: u64,
    /// `event` | `state` | `permission`
    pub kind: String,
    pub interaction_state: String,
    pub execution_state: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn host_path(host_id: &str, path: &Path) -> HostPath {
    let display = super::super::terminal::model::canonical_display(path);
    HostPath {
        host_id: host_id.to_string(),
        display_path: display.display().to_string(),
        canonical_path: super::super::terminal::model::canonical_identity(path),
    }
}

/// Maps a display/canonical pair onto a filesystem path, preferring the
/// canonical spelling when the discovery layer captured one.
///
/// A verbatim (`\\?\`) prefix is stripped: it is a valid filesystem path but
/// `cmd.exe` cannot execute one, so a `.cmd`/`.bat` target registered with a
/// verbatim identity fails with "The system cannot find the path specified"
/// before the protocol even starts. S04 hit the same trap when a private probe
/// registered an un-normalised path.
pub fn installation_path(executable: &HostPath) -> std::path::PathBuf {
    strip_verbatim(
        executable
            .canonical_path
            .clone()
            .unwrap_or_else(|| executable.display_path.clone()),
    )
}

/// Removes a Windows verbatim prefix, leaving UNC and drive paths intact.
///
/// This is **launch-only**: it changes the path handed to the launcher, never
/// the registered `HostPath` identity. Stripping here must not hide a
/// mismatched registration — `is_file` and the executable fingerprint are still
/// taken against the stored path, so an installation that points somewhere else
/// keeps failing for the right reason.
pub fn strip_verbatim(value: String) -> std::path::PathBuf {
    let trimmed = if let Some(rest) = value.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = value.strip_prefix("\\\\?\\") {
        rest.to_string()
    } else {
        value
    };
    std::path::PathBuf::from(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_request() -> RuntimeStartRequest {
        RuntimeStartRequest {
            installation_id: "inst-1".into(),
            cwd: std::env::temp_dir().display().to_string(),
            prompt: None,
            title: None,
            permission_timeout_ms: None,
            model: None,
            effort: None,
        }
    }

    #[test]
    fn products_map_to_the_frozen_agent_ids() {
        assert_eq!(RuntimeProduct::Codex.agent_id(), "codex");
        assert_eq!(RuntimeProduct::ClaudeCode.agent_id(), "claude");
        assert_eq!(RuntimeProduct::CodeBuddy.agent_id(), "codebuddy");
        assert_eq!(
            RuntimeProduct::for_agent("claude").map(|p| p.product_key()),
            Some("claude-code")
        );
        assert_eq!(RuntimeProduct::for_agent("gemini"), None);
    }

    #[test]
    fn start_request_rejects_unbounded_and_relative_inputs() {
        let mut request = base_request();
        assert!(request.validate().is_ok());

        request.cwd = "relative".into();
        assert!(request.validate().is_err());
        request.cwd = std::env::temp_dir().display().to_string();

        request.installation_id = "  ".into();
        assert!(request.validate().is_err());
        request = base_request();

        request.prompt = Some("a\0b".into());
        assert!(request.validate().is_err());
        request.prompt = Some("x".repeat(MAX_PROMPT_BYTES + 1));
        assert!(request.validate().is_err());
        request.prompt = Some("你好 😀".into());
        assert!(request.validate().is_ok());

        request.permission_timeout_ms = Some(MIN_PERMISSION_TIMEOUT_MS - 1);
        assert!(request.validate().is_err());
        request.permission_timeout_ms = Some(MAX_PERMISSION_TIMEOUT_MS + 1);
        assert!(request.validate().is_err());
        request.permission_timeout_ms = Some(60_000);
        assert!(request.validate().is_ok());
    }

    #[test]
    fn read_and_answer_requests_are_bounded_and_closed() {
        let ok: RuntimeReadRequest =
            serde_json::from_value(serde_json::json!({ "runId": "r-1" })).unwrap();
        assert_eq!(ok.limit(), DEFAULT_EVENT_BATCH as usize);
        assert!(ok.validate().is_ok());

        let bad: RuntimeReadRequest = serde_json::from_value(
            serde_json::json!({ "runId": "r-1", "maxEvents": MAX_EVENT_BATCH + 1 }),
        )
        .unwrap();
        assert!(bad.validate().is_err());

        assert!(serde_json::from_value::<RuntimeReadRequest>(
            serde_json::json!({ "runId": "r-1", "extra": 1 })
        )
        .is_err());

        let answer: RuntimeAnswerRequest = serde_json::from_value(serde_json::json!({
            "runId": "r-1",
            "permissionId": "perm-1",
            "optionId": "reject_once",
        }))
        .unwrap();
        assert!(answer.validate().is_ok());
        assert!(answer.validate().is_ok());
    }

    #[test]
    fn verbatim_paths_are_normalised_before_a_launch() {
        // `cmd.exe` cannot execute a verbatim path; the runtime must hand it a
        // plain one even if an installation was registered with a verbatim
        // identity.
        assert_eq!(
            strip_verbatim(r"\\?\C:\Users\me\codebuddy.cmd".to_string()),
            std::path::PathBuf::from(r"C:\Users\me\codebuddy.cmd")
        );
        assert_eq!(
            strip_verbatim(r"\\?\UNC\server\share\a.exe".to_string()),
            std::path::PathBuf::from(r"\\server\share\a.exe")
        );
        // A normal path is untouched.
        assert_eq!(
            strip_verbatim(r"C:\Program Files\a.exe".to_string()),
            std::path::PathBuf::from(r"C:\Program Files\a.exe")
        );
    }

    #[test]
    fn tool_quadrants_never_fold_silence_into_denial() {
        let base = RuntimeToolView {
            tool_call_id: "t".into(),
            permission_id: None,
            authorization: "not-requested".into(),
            execution: "executed".into(),
            outcome: "ok".into(),
            name: "Read".into(),
            kind: "search".into(),
            status: "completed".into(),
            input: None,
            output: None,
            is_error: false,
            started_at: "t0".into(),
            ended_at: Some("t1".into()),
        };
        assert_eq!(base.quadrant(), "never-asked-executed-ok");

        // Never asked, executed, and it failed: the most dangerous cell,
        // because the user was never consulted.
        let silent_failure = RuntimeToolView {
            outcome: "failed".into(),
            ..base.clone()
        };
        assert_eq!(silent_failure.quadrant(), "never-asked-executed-failed");

        let denied = RuntimeToolView {
            authorization: "denied".into(),
            execution: "not-executed".into(),
            outcome: "unknown".into(),
            ..base.clone()
        };
        assert_eq!(denied.quadrant(), "denied-not-executed");

        let allowed = RuntimeToolView {
            authorization: "allowed".into(),
            ..base.clone()
        };
        assert_eq!(allowed.quadrant(), "allowed-executed-ok");

        // Allowed by the user, executed, and it failed anyway.
        let allowed_failed = RuntimeToolView {
            authorization: "allowed".into(),
            outcome: "failed".into(),
            ..base.clone()
        };
        assert_eq!(allowed_failed.quadrant(), "allowed-executed-failed");

        // Allowed, but the call never reported a result: the ledger must be
        // able to say this instead of leaving it "running" forever.
        let allowed_but_nothing = RuntimeToolView {
            authorization: "allowed-always".into(),
            execution: "not-executed".into(),
            outcome: "unknown".into(),
            ..base.clone()
        };
        assert_eq!(allowed_but_nothing.quadrant(), "allowed-not-executed");

        let pending = RuntimeToolView {
            authorization: "pending".into(),
            execution: "unknown".into(),
            outcome: "unknown".into(),
            ..base
        };
        assert_eq!(pending.quadrant(), "unknown");
    }

    #[test]
    fn default_permission_timeout_is_answerable_but_finite() {
        let request = base_request();
        assert_eq!(
            request.permission_timeout(),
            std::time::Duration::from_millis(DEFAULT_PERMISSION_TIMEOUT_MS)
        );
        assert!(DEFAULT_PERMISSION_TIMEOUT_MS >= MIN_PERMISSION_TIMEOUT_MS);
        assert!(DEFAULT_PERMISSION_TIMEOUT_MS <= MAX_PERMISSION_TIMEOUT_MS);
    }
}
