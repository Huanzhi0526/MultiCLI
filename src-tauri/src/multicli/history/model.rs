//! Wire models for the S03 history contract (`docs/contracts/history-v1.md`).
//!
//! These are separate from the frozen S01 domain DTOs: a history view adds
//! provenance, cursor and search fields without changing the frozen shapes.
//! Every request validates its own bounds so an IPC call can never ask the
//! backend for an unbounded scan or an unbounded page.

use crate::multicli::domain::{HostPath, Message, Run, Session, SessionWorkspaceRole, MAX_PAGE_LIMIT};
use crate::multicli::error::AppError;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

pub const HISTORY_SCHEMA_VERSION: u32 = 1;

/// Index parser generation. Bumping it makes every stored cursor stale, which
/// is the honest response to a mapping change: re-read rather than reinterpret.
pub const HISTORY_PARSER_VERSION: u32 = 1;

/// A single JSONL line larger than this is imported in truncated form with an
/// explicit flag instead of being dropped or buffered without bound.
pub const MAX_LINE_BYTES: usize = 1_048_576;
/// Imported text per message is capped independently of the line cap.
pub const MAX_MESSAGE_TEXT_BYTES: usize = 262_144;
/// Search body per message. Larger bodies are truncated with `truncated: true`.
pub const MAX_SEARCH_BODY_BYTES: usize = 65_536;
/// Directory depth below a configured root.
pub const MAX_SCAN_DEPTH: usize = 12;
/// Files listed per source per pass.
pub const MAX_FILES_PER_PASS: usize = 50_000;
/// Bytes read from one file per source pass.
pub const MAX_BYTES_PER_FILE_PASS: u64 = 32 * 1024 * 1024;
/// Wall-clock budget for one source pass.
pub const PASS_BUDGET_MS: u64 = 4_000;
/// Rebuild gets a larger budget because it is an explicit user action.
pub const REBUILD_BUDGET_MS: u64 = 30_000;
/// Automatic incremental sweep interval.
pub const SWEEP_INTERVAL_MS: u64 = 30_000;
/// Wall-clock budget for one file's import transaction. A file larger than
/// this budget is committed in several passes; the alternative is holding the
/// only database connection (and therefore the UI) for a whole pass.
pub const FILE_BUDGET_MS: u64 = 1_500;
/// Content-verification block size. 256 KiB keeps the per-block hash small and
/// the verification window cheap on a cold cache.
pub const BLOCK_BYTES: u64 = 262_144;
/// Bytes of one file's blocks verified per pass.
pub const VERIFY_WINDOW_BYTES: u64 = 1_048_576;
/// Total verification bytes one pass may spend across all files. A large corpus
/// is therefore verified over several passes instead of re-reading everything.
pub const VERIFY_BUDGET_PER_PASS: u64 = 8 * 1024 * 1024;
/// A file that finished a full verification cycle is not re-verified until this
/// cooldown expires, so one pass's budget cannot starve the rest of a corpus.
pub const VERIFY_COOLDOWN_MS: i64 = 15 * 60 * 1000;
/// Slice of a pass reserved for verification. Imports otherwise consume the
/// whole budget and verification never runs at all; this is what makes the
/// bound on an unchanged prefix independent of how much new data arrives.
pub const VERIFY_RESERVE_MS: u64 = 400;
/// Largest compressed transcript accepted, measured on the bytes on disk.
///
/// The limit exists because the physical baseline is built by reading the whole
/// compressed file inside one bounded transaction: at 128 MiB the read needs
/// about 85 MB/s to finish inside the 1.5 s budget, which every local disk
/// reaches and which still leaves room for the decode. A file above it is
/// rejected *before* decoding, with its previous rows untouched; refusing to
/// import is honest, whereas importing and then never being able to verify
/// would leave the user with a promise the scanner cannot keep. 128 MiB of
/// compressed data is typically several hundred megabytes of decoded
/// transcript, comfortably above the decompression cap below.
pub const MAX_COMPRESSED_PHYSICAL_BYTES: u64 = 128 * 1024 * 1024;
/// Decompressed bytes one compressed transcript may produce per pass. Bounds a
/// decompression bomb without disabling `.zst` transcripts.
pub const MAX_DECOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
/// Bytes of a single over-long line that are skipped before the file is
/// rejected. Bounds a pathological record instead of scanning it to the end.
pub const MAX_LINE_SKIP_BYTES: usize = 16 * 1024 * 1024;
/// Lines inspected before a file is declared to be the wrong product.
pub const FORMAT_PROBE_LINES: u32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryProduct {
    pub product: &'static str,
    pub agent_id: &'static str,
    pub display_name: &'static str,
    /// Path components below the user's home directory.
    pub home_components: &'static [&'static str],
    /// Extensions that can contain records of this product.
    pub extensions: &'static [&'static str],
}

pub const PRODUCTS: &[HistoryProduct] = &[
    HistoryProduct {
        product: "codex",
        agent_id: "codex",
        display_name: "Codex CLI",
        home_components: &[".codex", "sessions"],
        extensions: &["jsonl", "jsonl.zst"],
    },
    HistoryProduct {
        product: "claude-code",
        agent_id: "claude",
        display_name: "Claude Code",
        home_components: &[".claude", "projects"],
        extensions: &["jsonl"],
    },
    HistoryProduct {
        product: "codebuddy-code",
        agent_id: "codebuddy",
        display_name: "CodeBuddy Code",
        home_components: &[".codebuddy", "projects"],
        extensions: &["jsonl"],
    },
];

pub fn product_for_key(key: &str) -> Option<&'static HistoryProduct> {
    let key = key.trim().to_ascii_lowercase();
    PRODUCTS.iter().find(|product| {
        product.product == key
            || product.agent_id == key
            || (key == "claude" && product.agent_id == "claude")
            || (key == "codebuddy" && product.agent_id == "codebuddy")
    })
}

pub fn product_for_agent(agent_id: &str) -> Option<&'static HistoryProduct> {
    PRODUCTS.iter().find(|product| product.agent_id == agent_id)
}

/// Namespace shared by every configured root of one product profile. Sessions
/// are keyed by `(namespace, nativeId)`, so moving a transcript between two
/// configured roots of the same profile keeps one session and one id.
pub fn source_namespace(product: &HistoryProduct, config_profile_id: &str) -> String {
    format!(
        "history:{}:{}:{}",
        product.agent_id, product.product, config_profile_id
    )
}

// ---------------------------------------------------------------------------
// Sources and status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryStatusView {
    pub generation: u64,
    /// `idle | indexing | ok | partial | error | cancelled`
    pub status: String,
    pub in_flight: bool,
    pub last_attempt_at: Option<String>,
    pub last_success_at: Option<String>,
    pub error: Option<AppError>,
    pub files_total: u64,
    pub files_done: u64,
    pub files_skipped: u64,
    pub files_failed: u64,
    pub messages_indexed: u64,
    pub bytes_read: u64,
    /// True only when the last pass walked the whole configured tree.
    pub complete: bool,
    pub progress_permille: u32,
    pub note: String,
}

impl HistoryStatusView {
    pub fn idle() -> Self {
        Self {
            generation: 0,
            status: "idle".to_string(),
            in_flight: false,
            last_attempt_at: None,
            last_success_at: None,
            error: None,
            files_total: 0,
            files_done: 0,
            files_skipped: 0,
            files_failed: 0,
            messages_indexed: 0,
            bytes_read: 0,
            complete: false,
            progress_permille: 0,
            note: STATUS_NOTE.to_string(),
        }
    }
}

pub const STATUS_NOTE: &str =
    "A pass walks only the configured root within depth, file, byte and time budgets. A file that is still being written is imported up to its last complete line; the tail is re-read on the next pass.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistorySourceView {
    pub source_id: String,
    pub host_id: String,
    pub agent_id: String,
    pub product: String,
    pub installation_id: Option<String>,
    pub config_profile_id: String,
    pub config_profile_status: String,
    pub root: HostPath,
    pub enabled: bool,
    pub parser_version: u32,
    pub created_at: String,
    pub updated_at: String,
    pub status: HistoryStatusView,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistorySourceStatus {
    pub source_id: String,
    pub agent_id: String,
    pub product: String,
    pub enabled: bool,
    pub status: HistoryStatusView,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryOverallStatus {
    pub schema_version: u32,
    pub scope: String,
    pub in_flight: bool,
    pub sources: Vec<HistorySourceStatus>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryCandidateView {
    pub agent_id: String,
    pub product: String,
    pub display_name: String,
    pub root: HostPath,
    pub exists: bool,
    pub readable: bool,
    /// Bounded sample of entries directly below the root (not a full count).
    pub entry_sample: u64,
    pub suggestion: String,
    pub already_configured: bool,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistorySourceUpdate {
    #[serde(default)]
    pub source_id: Option<String>,
    pub product: String,
    #[serde(default)]
    pub installation_id: Option<String>,
    pub config_profile_id: String,
    pub root: String,
    pub enabled: bool,
}

impl HistorySourceUpdate {
    pub fn validate(&self) -> Result<&'static HistoryProduct, AppError> {
        if let Some(source_id) = &self.source_id {
            if source_id.trim().is_empty() {
                return Err(AppError::invalid_argument("sourceId", "must not be empty"));
            }
        }
        let product = product_for_key(&self.product).ok_or_else(|| {
            AppError::invalid_argument(
                "product",
                "must be one of codex, claude-code, codebuddy-code",
            )
        })?;
        if let Some(installation_id) = &self.installation_id {
            if installation_id.trim().is_empty() {
                return Err(AppError::invalid_argument(
                    "installationId",
                    "must not be empty when present",
                ));
            }
        }
        if self.config_profile_id.trim().is_empty() {
            return Err(AppError::invalid_argument(
                "configProfileId",
                "must not be empty",
            ));
        }
        if self.config_profile_id.chars().count() > 64 {
            return Err(AppError::invalid_argument(
                "configProfileId",
                "must be at most 64 characters",
            ));
        }
        if self.root.trim().is_empty() {
            return Err(AppError::invalid_argument("root", "must not be empty"));
        }
        if self.root.chars().count() > 1024 {
            return Err(AppError::invalid_argument("root", "is too long"));
        }
        Ok(product)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryRefreshQuery {
    #[serde(default)]
    pub source_id: Option<String>,
}

impl HistoryRefreshQuery {
    pub fn validate(&self) -> Result<(), AppError> {
        optional_id("sourceId", &self.source_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryRebuildQuery {
    pub source_id: String,
}

impl HistoryRebuildQuery {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.source_id.trim().is_empty() {
            return Err(AppError::invalid_argument("sourceId", "must not be empty"));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Session metadata and bounded messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionWorkspaceDetail {
    pub workspace_id: String,
    pub role: SessionWorkspaceRole,
    pub evidence: String,
    pub path: Option<HostPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageKindCount {
    pub kind: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryProvenanceView {
    pub source_ids: Vec<String>,
    pub file_count: u64,
    pub parser_versions: Vec<u32>,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionMetadataView {
    pub session: Session,
    pub workspaces: Vec<SessionWorkspaceDetail>,
    pub runs: Vec<Run>,
    pub run_total: u64,
    pub message_count: u64,
    pub message_kinds: Vec<MessageKindCount>,
    pub first_message_at: Option<String>,
    pub last_message_at: Option<String>,
    pub history: Option<HistoryProvenanceView>,
    pub note: String,
}

pub const METADATA_NOTE: &str =
    "Metadata never includes the message list; messages are read through the bounded paged command. Run rows are capped at 50 and are read-only history rows, not live connections.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessagePageQuery {
    pub session_id: String,
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub anchor_message_id: Option<String>,
}

impl MessagePageQuery {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.session_id.trim().is_empty() {
            return Err(AppError::invalid_argument(
                "sessionId",
                "must not be empty",
            ));
        }
        validate_limit(self.limit)?;
        optional_id("cursor", &self.cursor)?;
        optional_id("anchorMessageId", &self.anchor_message_id)?;
        if self.cursor.is_some() && self.anchor_message_id.is_some() {
            return Err(AppError::invalid_argument(
                "anchorMessageId",
                "cannot be combined with cursor",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessagePage {
    pub items: Vec<Message>,
    pub next_cursor: Option<String>,
    pub total: u64,
    /// Echoed so the UI can scroll to the message it asked to anchor on.
    pub anchor_message_id: Option<String>,
    pub anchor_index: Option<u32>,
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistorySearchQuery {
    pub text: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub source_id: Option<String>,
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl HistorySearchQuery {
    pub fn validate(&self) -> Result<(), AppError> {
        let trimmed = self.text.trim();
        if trimmed.is_empty() {
            return Err(AppError::invalid_argument("text", "must not be empty"));
        }
        if trimmed.chars().count() > 200 {
            return Err(AppError::invalid_argument(
                "text",
                "must be at most 200 characters",
            ));
        }
        optional_id("workspaceId", &self.workspace_id)?;
        optional_id("agentId", &self.agent_id)?;
        optional_id("sourceId", &self.source_id)?;
        validate_limit(self.limit)?;
        optional_id("cursor", &self.cursor)?;
        Ok(())
    }

    pub fn normalized_text(&self) -> String {
        self.text.trim().to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchHitView {
    pub session_id: String,
    pub message_id: String,
    pub session_title: String,
    pub agent_id: String,
    pub kind: String,
    pub seq: u64,
    pub observed_at: String,
    pub snippet: String,
    pub snippet_truncated: bool,
    /// `fts` for a trigram match, `like` for the short-query fallback.
    pub match_mode: String,
    pub source_ids: Vec<String>,
    /// Total number of matches for this query, identical to the page `total`.
    pub session_message_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchPage {
    pub items: Vec<SearchHitView>,
    pub next_cursor: Option<String>,
    pub total: u64,
    pub match_mode: String,
    pub note: String,
}

pub const SEARCH_NOTE: &str =
    "Search runs in the local SQLite FTS5 index. Queries of three or more characters use the trigram index (substring, including Chinese); one- and two-character queries use an escaped LIKE fallback. Results contain plain text only and are rendered as text, never as HTML.";

// ---------------------------------------------------------------------------
// Resume target
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResumeConflictView {
    /// `active-run` | `live-external-process` | `missing-native-id`
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResumeTargetView {
    pub session_id: String,
    pub agent_id: String,
    pub product: Option<String>,
    pub native_id: Option<String>,
    pub source_namespace: String,
    pub config_profile_id: Option<String>,
    /// `resolved | blocked | unknown | unsupported`
    pub status: String,
    pub reason: String,
    pub installation_id: Option<String>,
    pub executable: Option<HostPath>,
    pub workspaces: Vec<HostPath>,
    pub conflicts: Vec<ResumeConflictView>,
    pub evidence: Vec<String>,
    /// Execution is S05; this stage only ever parses a target.
    pub execution_available: bool,
    pub note: String,
}

pub const RESUME_NOTE: &str =
    "This command only resolves a target. It does not start a process, send a prompt, write a CLI file or take a lease; opening a history session is not a resume.";

// ---------------------------------------------------------------------------
// Cursors
// ---------------------------------------------------------------------------

/// A message-page cursor is a position *inside one session*. Two sessions can
/// produce the same (observedAt, seq, id) triple by coincidence, and reusing a
/// cursor across them silently skipped records, so the session is part of the
/// cursor and is verified on decode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MessageCursorPayload {
    sort: String,
    session_id: String,
    observed_at: String,
    seq: u64,
    id: String,
    /// Purge counter at the time the page was produced. A rebuild removes and
    /// re-imports rows, so a cursor from before it may point into a different
    /// timeline.
    epoch: u64,
}

pub(crate) fn encode_message_cursor(
    session_id: &str,
    observed_at: &str,
    seq: u64,
    id: &str,
    epoch: u64,
) -> String {
    let payload = MessageCursorPayload {
        sort: "history-messages".to_string(),
        session_id: session_id.to_string(),
        observed_at: observed_at.to_string(),
        seq,
        id: id.to_string(),
        epoch,
    };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap_or_default())
}

pub(crate) fn decode_message_cursor(
    cursor: &str,
    session_id: &str,
    epoch: u64,
) -> Result<crate::multicli::repo::MessagePosition, AppError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| AppError::invalid_argument("cursor", "is not a valid pagination cursor"))?;
    let payload: MessageCursorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::invalid_argument("cursor", "is not a valid pagination cursor"))?;
    if payload.sort != "history-messages" {
        return Err(AppError::invalid_argument(
            "cursor",
            "belongs to a different list",
        ));
    }
    if payload.session_id != session_id {
        return Err(AppError::invalid_argument(
            "cursor",
            "belongs to a different session",
        ));
    }
    if payload.epoch != epoch {
        return Err(AppError::invalid_argument(
            "cursor",
            "predates an index rebuild; reload the session",
        ));
    }
    Ok(crate::multicli::repo::MessagePosition {
        observed_at: payload.observed_at,
        seq: payload.seq,
        id: payload.id,
    })
}

/// Everything a search cursor is only valid for.
///
/// A cursor is a position in one result set: the same page means nothing under
/// a different keyword, filter set or match mode, and a rebuild can remove the
/// rows behind it. Carrying a fingerprint of all four turns a misuse into an
/// error instead of a silently short page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchBinding {
    pub query: String,
    pub filters: String,
    pub mode: String,
    pub epoch: u64,
}

impl SearchBinding {
    pub fn new(
        text: &str,
        workspace_id: Option<&str>,
        agent_id: Option<&str>,
        source_id: Option<&str>,
        mode: &str,
        epoch: u64,
    ) -> Self {
        let query = crate::multicli::history::ids::stable_id("hq", &[text]);
        let filters = crate::multicli::history::ids::stable_id(
            "hf",
            &[
                workspace_id.unwrap_or(""),
                agent_id.unwrap_or(""),
                source_id.unwrap_or(""),
            ],
        );
        Self {
            query,
            filters,
            mode: mode.to_string(),
            epoch,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SearchCursorPayload {
    sort: String,
    session_created_at: i64,
    observed_at: String,
    seq: u64,
    id: String,
    query: String,
    filters: String,
    mode: String,
    epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPosition {
    pub session_created_at: i64,
    pub observed_at: String,
    pub seq: u64,
    pub id: String,
}

pub(crate) fn encode_search_cursor(position: &SearchPosition, binding: &SearchBinding) -> String {
    let payload = SearchCursorPayload {
        sort: "history-search".to_string(),
        session_created_at: position.session_created_at,
        observed_at: position.observed_at.clone(),
        seq: position.seq,
        id: position.id.clone(),
        query: binding.query.clone(),
        filters: binding.filters.clone(),
        mode: binding.mode.clone(),
        epoch: binding.epoch,
    };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap_or_default())
}

pub(crate) fn decode_search_cursor(
    cursor: &str,
    expected: &SearchBinding,
) -> Result<SearchPosition, AppError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| AppError::invalid_argument("cursor", "is not a valid pagination cursor"))?;
    let payload: SearchCursorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::invalid_argument("cursor", "is not a valid pagination cursor"))?;
    if payload.sort != "history-search" {
        return Err(AppError::invalid_argument(
            "cursor",
            "belongs to a different list",
        ));
    }
    if payload.query != expected.query {
        return Err(AppError::invalid_argument(
            "cursor",
            "belongs to a different search query",
        ));
    }
    if payload.filters != expected.filters || payload.mode != expected.mode {
        return Err(AppError::invalid_argument(
            "cursor",
            "belongs to a different filter set",
        ));
    }
    if payload.epoch != expected.epoch {
        return Err(AppError::invalid_argument(
            "cursor",
            "predates an index rebuild; start the search again",
        ));
    }
    Ok(SearchPosition {
        session_created_at: payload.session_created_at,
        observed_at: payload.observed_at,
        seq: payload.seq,
        id: payload.id,
    })
}

fn validate_limit(limit: u32) -> Result<(), AppError> {
    if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
        return Err(AppError::invalid_argument(
            "limit",
            format!("must be between 1 and {MAX_PAGE_LIMIT}"),
        ));
    }
    Ok(())
}

fn optional_id(field: &str, value: &Option<String>) -> Result<(), AppError> {
    if let Some(text) = value {
        if text.trim().is_empty() {
            return Err(AppError::invalid_argument(field, "must not be empty"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multicli::error::ErrorCode;

    #[test]
    fn product_keys_map_to_the_three_cores() {
        for (key, agent) in [
            ("codex", "codex"),
            ("claude", "claude"),
            ("claude-code", "claude"),
            ("codebuddy", "codebuddy"),
            ("codebuddy-code", "codebuddy"),
        ] {
            assert_eq!(
                product_for_key(key).map(|product| product.agent_id),
                Some(agent),
                "{key}"
            );
        }
        assert!(product_for_key("gemini").is_none());
    }

    #[test]
    fn request_bounds_reject_unbounded_reads() {
        for limit in [0_u32, 201] {
            let query = MessagePageQuery {
                session_id: "s".into(),
                limit,
                cursor: None,
                anchor_message_id: None,
            };
            assert_eq!(query.validate().unwrap_err().code, ErrorCode::InvalidArgument);
        }
        let both = MessagePageQuery {
            session_id: "s".into(),
            limit: 20,
            cursor: Some("c".into()),
            anchor_message_id: Some("m".into()),
        };
        assert!(both.validate().is_err());
        let search = HistorySearchQuery {
            text: "   ".into(),
            workspace_id: None,
            agent_id: None,
            source_id: None,
            limit: 20,
            cursor: None,
        };
        assert!(search.validate().is_err());
        let long = HistorySearchQuery {
            text: "中".repeat(201),
            workspace_id: None,
            agent_id: None,
            source_id: None,
            limit: 20,
            cursor: None,
        };
        assert!(long.validate().is_err());
        let unknown = serde_json::from_value::<HistorySearchQuery>(serde_json::json!({
            "text": "x", "limit": 20, "extra": true
        }));
        assert!(unknown.is_err());
    }

    #[test]
    fn cursors_are_list_specific_and_round_trip() {
        let cursor = encode_message_cursor("s-1", "2026-11-02T01:00:00.000Z", 7, "m-1", 3);
        let position = decode_message_cursor(&cursor, "s-1", 3).unwrap();
        assert_eq!(position.seq, 7);
        assert_eq!(position.id, "m-1");
        assert!(decode_search_cursor(&cursor, &binding()).is_err());
        assert!(
            decode_message_cursor(&cursor, "s-2", 3).is_err(),
            "a message cursor belongs to one session"
        );
        assert!(
            decode_message_cursor(&cursor, "s-1", 4).is_err(),
            "a rebuild invalidates the page position"
        );
        let search = encode_search_cursor(
            &SearchPosition {
                session_created_at: 5,
                observed_at: "2026-11-02T01:00:00.000Z".into(),
                seq: 2,
                id: "m-2".into(),
            },
            &binding(),
        );
        let position = decode_search_cursor(&search, &binding()).unwrap();
        assert_eq!(position.session_message_position(), (5, 2));
        assert!(decode_message_cursor(&search, "s-1", 3).is_err());
        assert!(decode_message_cursor("not base64", "s-1", 3).is_err());
    }

    fn binding() -> SearchBinding {
        SearchBinding::new("hello", None, None, None, "fts", 3)
    }

    #[test]
    fn a_search_cursor_is_bound_to_its_query_filters_mode_and_epoch() {
        let position = SearchPosition {
            session_created_at: 5,
            observed_at: "2026-11-02T01:00:00.000Z".into(),
            seq: 2,
            id: "m-2".into(),
        };
        let base = binding();
        let cursor = encode_search_cursor(&position, &base);
        assert!(decode_search_cursor(&cursor, &base).is_ok());
        for other in [
            SearchBinding::new("hello2", None, None, None, "fts", 3),
            SearchBinding::new("hello", Some("w"), None, None, "fts", 3),
            SearchBinding::new("hello", None, Some("codex"), None, "fts", 3),
            SearchBinding::new("hello", None, None, None, "like", 3),
            SearchBinding::new("hello", None, None, None, "fts", 4),
        ] {
            let error = decode_search_cursor(&cursor, &other).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument);
        }
    }

    impl SearchPosition {
        fn session_message_position(&self) -> (i64, u64) {
            (self.session_created_at, self.seq)
        }
    }
}
