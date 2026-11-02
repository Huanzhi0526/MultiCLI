//! Per-product history parsers.
//!
//! Codex, Claude Code and CodeBuddy write three different JSONL dialects with
//! different envelope keys, session identity and timestamps. They share the
//! normalisation layer below (bounded text, whitelisted content, deterministic
//! source keys) but each parser maps its own records: reusing one parser with a
//! different product name would silently invent fields that the product does
//! not write.

mod claude;
mod codebuddy;
mod codex;

pub use claude::ClaudeParser;
pub use codebuddy::CodeBuddyParser;
pub use codex::CodexParser;

use crate::multicli::history::model::{
    product_for_agent, HistoryProduct, MAX_MESSAGE_TEXT_BYTES, MAX_SEARCH_BODY_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Everything a pass must remember about a file prefix so that a later pass can
/// continue the same session.
///
/// Without it an append starts with no native session id, invents one from the
/// file name, and turns one transcript into two sessions. The blob is stored on
/// the cursor and restored before the parser sees the first appended line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParserCheckpoint {
    pub native_session_id: Option<String>,
    pub cwd: Option<String>,
    pub extra_roots: Vec<String>,
    pub pending_title: Option<PendingTitle>,
    /// True once a title candidate was taken, so a later user message does not
    /// rename an existing session on every pass.
    pub title_seen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingTitle {
    pub title: String,
    /// `native-title` | `first-user-message` | `none`
    pub source: String,
}

/// A parse failure carries a fixed reason and a line/byte location — never the
/// line content, which may contain prompts, tokens or paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineError {
    pub code: &'static str,
    pub detail: String,
}

impl LineError {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

#[allow(dead_code)]
pub struct ParseContext<'a> {
    pub line_index: u64,
    pub byte_offset: u64,
    /// Canonical ISO timestamp of the file's mtime. Used as `observedAt` only
    /// when the record itself carries no time, and then marked as such.
    pub file_mtime: &'a str,
    /// File stem, used as a last-resort native session id (every supported
    /// product names the transcript after the session).
    pub file_stem: &'a str,
    /// True when the physical line exceeded the byte cap and was clipped.
    pub line_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub native_id: String,
    pub title: Option<String>,
    pub title_source: TitleSource,
    pub cwd: Option<String>,
    pub extra_roots: Vec<String>,
    pub started_at: Option<String>,
    pub product_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSource {
    NativeTitle,
    FirstUserMessage,
    None,
}

impl TitleSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NativeTitle => "native-title",
            Self::FirstUserMessage => "first-user-message",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordOut {
    pub native_session_id: String,
    pub native_event_id: Option<String>,
    pub source_key: String,
    pub kind: String,
    pub seq: u64,
    pub timestamp: Option<String>,
    pub content: Value,
    pub search_text: String,
    pub cwd: Option<String>,
    pub extra_roots: Vec<String>,
    pub title_update: Option<(String, TitleSource)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Meta(SessionMeta),
    /// A native title record. It updates the session title but is not a chat
    /// message, so it never becomes an empty row in the timeline.
    Title {
        title: String,
        source: TitleSource,
    },
    Record(RecordOut),
    /// Record is understood but deliberately not shown (token counters,
    /// snapshots, editor state). It still counts as a successfully read line.
    Ignored,
}

#[allow(dead_code)]
pub trait HistoryParser: Send {
    fn product(&self) -> &'static HistoryProduct;
    fn parse_line(
        &mut self,
        value: &Value,
        ctx: &ParseContext<'_>,
    ) -> Result<Vec<Parsed>, LineError>;

    fn agent_id(&self) -> &'static str {
        self.product().agent_id
    }

    fn matches_first_line(&self, value: &Value) -> bool;

    /// State derived from lines that a later pass will not read again.
    fn checkpoint(&self) -> ParserCheckpoint {
        ParserCheckpoint::default()
    }

    /// Restores [`HistoryParser::checkpoint`] before an appended range.
    fn restore(&mut self, checkpoint: &ParserCheckpoint) {
        let _ = checkpoint;
    }
}

pub fn parser_for_agent(agent_id: &str) -> Option<Box<dyn HistoryParser>> {
    match product_for_agent(agent_id)?.product {
        "codex" => Some(Box::new(CodexParser::default())),
        "claude-code" => Some(Box::new(ClaudeParser::default())),
        "codebuddy-code" => Some(Box::new(CodeBuddyParser::default())),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Shared normalisation helpers
// ---------------------------------------------------------------------------

/// Block kinds that are displayed by name only. Their payload is deliberately
/// not copied: an image body, an encrypted reasoning blob or a signature is not
/// a message.
pub fn block_placeholder(kind: &str) -> String {
    match kind {
        "image" | "input_image" => "[image]".to_string(),
        "thinking" | "reasoning" => "[reasoning]".to_string(),
        _ => format!("[{kind}]"),
    }
}

/// Extracts readable text from the string-or-array content shapes used by all
/// three products. Returns the text plus the block type names seen (names only,
/// so the UI can say "this record also had an image").
pub fn content_text(value: &Value) -> (String, Vec<String>) {
    match value {
        Value::String(text) => (text.clone(), vec!["text".to_string()]),
        Value::Array(items) => {
            let mut text = String::new();
            let mut kinds = Vec::new();
            for item in items {
                match item {
                    Value::String(line) => append_text(&mut text, line),
                    Value::Object(map) => {
                        let kind = map
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string();
                        if !kinds.contains(&kind) {
                            kinds.push(kind.clone());
                        }
                        match kind.as_str() {
                            "text" | "input_text" | "output_text" | "summary_text" => {
                                if let Some(line) = map.get("text").and_then(Value::as_str) {
                                    append_text(&mut text, line);
                                }
                            }
                            "thinking" | "reasoning" => {
                                if let Some(line) = map.get("thinking").and_then(Value::as_str) {
                                    append_text(&mut text, line);
                                } else if let Some(line) =
                                    map.get("text").and_then(Value::as_str)
                                {
                                    append_text(&mut text, line);
                                }
                            }
                            "tool_use" => {
                                if let Some(name) = map.get("name").and_then(Value::as_str) {
                                    append_text(&mut text, &format!("[tool: {name}]"));
                                }
                            }
                            "tool_result" => {
                                let (inner, _) = content_text(
                                    map.get("content").unwrap_or(&Value::Null),
                                );
                                append_text(&mut text, &inner);
                            }
                            other => append_text(&mut text, &block_placeholder(other)),
                        }
                    }
                    other => append_text(&mut text, &other.to_string()),
                }
            }
            (text, kinds)
        }
        Value::Null => (String::new(), Vec::new()),
        other => (other.to_string(), Vec::new()),
    }
}

pub fn append_text(target: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(text);
}

/// Clips to a byte budget on a character boundary and reports truncation.
pub fn clip_bytes(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_string(), false);
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// Tool input/arguments: untrusted, shown in a collapsed block. Control and
/// bidi characters are removed before the byte cap for the same reason message
/// text is sanitized: an escape sequence must not be able to reorder what the
/// reader sees.
pub fn tool_text(raw: &str, limit: usize) -> (String, bool) {
    let sanitized = crate::multicli::history::ids::sanitize_text(raw);
    clip_bytes(&sanitized, limit)
}

/// Builds the message text: sanitized, capped, and explicitly flagged.
pub fn message_text(raw: &str) -> (String, bool) {
    let sanitized = crate::multicli::history::ids::sanitize_text(raw);
    let (clipped, truncated) = clip_bytes(&sanitized, MAX_MESSAGE_TEXT_BYTES);
    (clipped.trim().to_string(), truncated)
}

pub fn search_body(raw: &str) -> String {
    let sanitized = crate::multicli::history::ids::sanitize_text(raw);
    let (clipped, _) = clip_bytes(&sanitized, MAX_SEARCH_BODY_BYTES);
    clipped.to_lowercase()
}

/// Title candidate from the first user message: single line, bounded.
///
/// Product-injected context arrives as a user/developer message whose whole
/// body is an XML-ish tag (`<external_codex_apps_open_page>…`). Using that as a
/// title would be technically faithful and practically useless, so tag-shaped
/// lines are skipped; the first real sentence wins. If every line is a tag, the
/// title stays empty rather than showing markup.
pub fn title_from_text(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| {
        !line.is_empty()
            && !line.starts_with('<')
            && !line.starts_with('[')
            && !(line.starts_with('{') && line.ends_with('}'))
    })?;
    let sanitized = crate::multicli::history::ids::sanitize_text(line);
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (clipped, _) = crate::multicli::history::ids::truncate_chars(trimmed, 120);
    Some(clipped)
}

/// Native `aiTitle` values are untrusted too: strip control/bidi characters and
/// bound the length before the value becomes a session title.
pub fn native_title(raw: &str) -> Option<String> {
    let sanitized = crate::multicli::history::ids::sanitize_text(raw);
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (clipped, _) = crate::multicli::history::ids::truncate_chars(trimmed, 200);
    Some(clipped)
}

/// Names of fields the parser did not consume. Values are never retained; the
/// name is filtered to a safe character set and length so a hostile key cannot
/// smuggle content into the "unknown fields" list.
pub fn unknown_field_names(value: &Value, known: &[&str]) -> Vec<String> {
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    let mut names: Vec<String> = map
        .keys()
        .filter(|key| !known.contains(&key.as_str()))
        .filter(|key| {
            !key.is_empty()
                && key.len() <= 40
                && key
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':'))
        })
        .cloned()
        .collect();
    names.sort();
    names.truncate(32);
    names
}

pub fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

pub fn string_list(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .take(16)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Deterministic fallback event id from the file location. Two identical
/// copies of a transcript produce the same key, so a duplicated file cannot
/// duplicate messages.
pub fn fallback_event_id(kind: &str, ctx: &ParseContext<'_>) -> String {
    format!("{}:{}", kind, ctx.line_index)
}
