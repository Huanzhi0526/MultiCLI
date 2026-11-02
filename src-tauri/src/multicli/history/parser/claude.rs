//! Claude Code transcript parser.
//!
//! Observed format (Claude Code 2.1.153, `%USERPROFILE%\.claude\projects\
//! <project-slug>\<session-uuid>.jsonl`): every record carries `sessionId`,
//! `uuid`, `parentUuid`, `cwd`, `timestamp`, `version` and usually
//! `message:{role, content}`. Assistant content is an array of blocks
//! (`text`, `thinking`, `tool_use`), user content is a string or an array that
//! may contain `tool_result`. `ai-title` and `summary` records are metadata, not
//! messages; `file-history-snapshot`, `mode` and `permission-mode` are editor
//! state and are ignored.
//!
//! One block becomes one message so a tool call and the assistant text that
//! introduced it are both visible in order, without inventing a combined body
//! the product never wrote.

use super::{
    block_placeholder, content_text, fallback_event_id, message_text, native_title, search_body,
    string_field, title_from_text, tool_text, unknown_field_names, HistoryParser, LineError,
    ParseContext, Parsed, ParserCheckpoint, RecordOut, TitleSource,
};
use crate::multicli::history::ids::{normalize_timestamp, stable_id};
use crate::multicli::history::model::{product_for_agent, HistoryProduct};
use serde_json::{json, Value};

#[derive(Default)]
pub struct ClaudeParser {
    native_id: Option<String>,
    cwd: Option<String>,
    title_seen: bool,
}

impl HistoryParser for ClaudeParser {
    fn product(&self) -> &'static HistoryProduct {
        product_for_agent("claude").expect("claude product is registered")
    }

    fn matches_first_line(&self, value: &Value) -> bool {
        let record_type = value.get("type").and_then(Value::as_str).unwrap_or_default();
        matches!(
            record_type,
            "mode" | "permission-mode" | "user" | "assistant" | "summary" | "system"
        ) && value.get("sessionId").and_then(Value::as_str).is_some()
    }

    fn checkpoint(&self) -> ParserCheckpoint {
        ParserCheckpoint {
            native_session_id: self.native_id.clone(),
            cwd: self.cwd.clone(),
            extra_roots: Vec::new(),
            pending_title: None,
            title_seen: self.title_seen,
        }
    }

    fn restore(&mut self, checkpoint: &ParserCheckpoint) {
        if self.native_id.is_none() {
            self.native_id = checkpoint.native_session_id.clone();
        }
        if self.cwd.is_none() {
            self.cwd = checkpoint.cwd.clone();
        }
        self.title_seen = self.title_seen || checkpoint.title_seen;
    }

    fn parse_line(
        &mut self,
        value: &Value,
        ctx: &ParseContext<'_>,
    ) -> Result<Vec<Parsed>, LineError> {
        let record_type = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| LineError::new("missing-record-type", "line has no type field"))?;
        if let Some(session_id) = string_field(value, "sessionId") {
            if self.native_id.is_none() {
                self.native_id = Some(session_id);
            }
        }
        if let Some(cwd) = string_field(value, "cwd") {
            self.cwd = Some(cwd);
        }
        let native_id = self
            .native_id
            .clone()
            .unwrap_or_else(|| stable_id("claude-file", &[ctx.file_stem]));
        let timestamp = value.get("timestamp").and_then(normalize_timestamp);
        let uuid = string_field(value, "uuid").unwrap_or_else(|| fallback_event_id(record_type, ctx));
        let seq = ctx.line_index;
        let cwd = self.cwd.clone();

        match record_type {
            "ai-title" => match native_title(&string_field(value, "aiTitle").unwrap_or_default()) {
                Some(title) => Ok(vec![Parsed::Title {
                    title,
                    source: TitleSource::NativeTitle,
                }]),
                None => Ok(vec![Parsed::Ignored]),
            },
            "summary" => {
                let text = string_field(value, "summary").unwrap_or_default();
                let mut out = base_record(&native_id, &uuid, seq, timestamp, cwd);
                out.kind = "summary".to_string();
                out.source_key = format!("claude:summary:{uuid}");
                out.native_event_id = Some(uuid);
                let (text, truncated) = message_text(&text);
                out.content = json!({
                    "role": "system",
                    "text": text,
                    "truncated": truncated,
                    "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    "unknownFields": unknown_field_names(
                        value,
                        &["type", "summary", "sessionId", "uuid", "parentUuid", "timestamp",
                          "cwd", "version", "gitBranch", "isSidechain", "userType"],
                    ),
                });
                out.search_text = search_body(&text);
                Ok(vec![Parsed::Record(out)])
            }
            "user" | "assistant" => Ok(self.message_record(value, ctx, &native_id, &uuid, seq, timestamp, cwd)),
            "system" => {
                let subtype = string_field(value, "subtype").unwrap_or_else(|| "system".to_string());
                let (raw_text, _) = content_text(value.get("content").unwrap_or(&Value::Null));
                let text = if raw_text.trim().is_empty() {
                    format!("[{subtype}]")
                } else {
                    raw_text
                };
                let mut out = base_record(&native_id, &uuid, seq, timestamp, cwd);
                out.kind = "system".to_string();
                out.source_key = format!("claude:system:{uuid}");
                out.native_event_id = Some(uuid);
                let (text, truncated) = message_text(&text);
                out.content = json!({
                    "role": "system",
                    "subtype": safe_token(&subtype),
                    "text": text,
                    "truncated": truncated,
                    "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    "unknownFields": unknown_field_names(
                        value,
                        &["type", "subtype", "content", "sessionId", "uuid", "parentUuid",
                          "timestamp", "cwd", "version", "gitBranch", "isSidechain", "userType",
                          "level", "slug", "isMeta", "compactMetadata", "logicalParentUuid",
                          "error", "maxRetries", "retryAttempt", "retryInMs", "durationMs",
                          "messageCount"],
                    ),
                });
                out.search_text = search_body(&text);
                Ok(vec![Parsed::Record(out)])
            }
            "attachment" => {
                let kind = value
                    .get("attachment")
                    .and_then(|attachment| attachment.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                let mut out = base_record(&native_id, &uuid, seq, timestamp, cwd);
                out.kind = "attachment".to_string();
                out.source_key = format!("claude:attachment:{uuid}");
                out.native_event_id = Some(uuid);
                out.content = json!({
                    "role": "user",
                    "text": block_placeholder(kind),
                    "blocks": [safe_token(kind)],
                    "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                });
                Ok(vec![Parsed::Record(out)])
            }
            // Editor/queue state with no conversation content.
            "mode" | "permission-mode" | "file-history-snapshot" | "last-prompt"
            | "queue-operation" => Ok(vec![Parsed::Ignored]),
            other => {
                let mut out = base_record(&native_id, &uuid, seq, timestamp, cwd);
                out.kind = "unknown".to_string();
                out.source_key = format!("claude:unknown:{}", fallback_event_id(other, ctx));
                out.content = json!({
                    "recordType": safe_token(other),
                    "text": "",
                    "unknownFields": unknown_field_names(&Value::Null, &[]),
                });
                Ok(vec![Parsed::Record(out)])
            }
        }
    }
}

impl ClaudeParser {
    #[allow(clippy::too_many_arguments)]
    fn message_record(
        &mut self,
        value: &Value,
        ctx: &ParseContext<'_>,
        native_id: &str,
        uuid: &str,
        seq: u64,
        timestamp: Option<String>,
        cwd: Option<String>,
    ) -> Vec<Parsed> {
        let role = value
            .get("message")
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            .unwrap_or(if value.get("type").and_then(Value::as_str) == Some("user") {
                "user"
            } else {
                "assistant"
            })
            .to_string();
        let content = value
            .get("message")
            .and_then(|message| message.get("content"))
            .cloned()
            .unwrap_or(Value::Null);
        let sidechain = value.get("isSidechain").and_then(Value::as_bool).unwrap_or(false);

        // A string body is one message. An array is split per block so tool
        // calls and their results keep their own identity and order.
        let blocks: Vec<(String, Value)> = match &content {
            Value::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let kind = item
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    (format!("{index}:{kind}"), item.clone())
                })
                .collect(),
            other => vec![("0:text".to_string(), json!({ "type": "text", "text": other }))],
        };

        let mut out_records = Vec::new();
        for (block_index, (label, block)) in blocks.into_iter().enumerate() {
            let block_kind = label.split(':').nth(1).unwrap_or("text").to_string();
            let event_id = format!("{uuid}#{block_index}");
            let mut out = base_record(native_id, &event_id, seq, timestamp.clone(), cwd.clone());
            out.native_event_id = Some(event_id.clone());
            out.source_key = format!("claude:message:{event_id}");
            out.content = match block_kind.as_str() {
                "tool_use" => {
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string();
                    let input = block
                        .get("input")
                        .map(|value| value.to_string())
                        .unwrap_or_default();
                    let (input, truncated) = tool_text(&input, 65_536);
                    out.kind = "tool-call".to_string();
                    out.search_text = search_body(&format!("{name} {input}"));
                    json!({
                        "role": "assistant",
                        "text": format!("[tool: {name}]"),
                        "tool": {
                            "name": safe_token(&name),
                            "callId": block.get("id").and_then(Value::as_str).map(safe_token),
                            "input": input,
                            "inputTruncated": truncated,
                        },
                        "sidechain": sidechain,
                        "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    })
                }
                "tool_result" => {
                    let (raw_text, blocks) =
                        content_text(block.get("content").unwrap_or(&Value::Null));
                    let (text, truncated) = message_text(&raw_text);
                    out.kind = "tool-result".to_string();
                    out.search_text = search_body(&text);
                    json!({
                        "role": "tool",
                        "text": text,
                        "blocks": blocks,
                        "tool": {
                            "callId": block.get("tool_use_id").and_then(Value::as_str).map(safe_token),
                        },
                        "truncated": truncated || ctx.line_truncated,
                        "sidechain": sidechain,
                        "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    })
                }
                "thinking" => {
                    let raw = block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let (text, truncated) = message_text(raw);
                    out.kind = "reasoning".to_string();
                    out.search_text = search_body(&text);
                    json!({
                        "role": "assistant",
                        "text": if text.is_empty() { block_placeholder("reasoning") } else { text },
                        "blocks": ["reasoning"],
                        "truncated": truncated,
                        "sidechain": sidechain,
                        "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    })
                }
                "image" => {
                    out.kind = "attachment".to_string();
                    json!({
                        "role": role,
                        "text": block_placeholder("image"),
                        "blocks": ["image"],
                        "sidechain": sidechain,
                        "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    })
                }
                "text" | "input_text" | "output_text" => {
                    let raw = block.get("text").and_then(Value::as_str).unwrap_or_default();
                    let (text, truncated) = message_text(raw);
                    out.kind = if role == "user" { "user-message" } else { "assistant-message" }
                        .to_string();
                    out.search_text = search_body(&text);
                    if role == "user" && !self.title_seen {
                        // Only the first user message is a title candidate; the
                        // caller still prefers a native title when one appears.
                        if let Some(title) = title_from_text(&text) {
                            self.title_seen = true;
                            out.title_update = Some((title, TitleSource::FirstUserMessage));
                        }
                    }
                    json!({
                        "role": role,
                        "text": text,
                        "blocks": ["text"],
                        "truncated": truncated || ctx.line_truncated,
                        "sidechain": sidechain,
                        "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    })
                }
                other => {
                    out.kind = "unknown".to_string();
                    json!({
                        "role": role,
                        "text": block_placeholder(other),
                        "blocks": [safe_token(other)],
                        "sidechain": sidechain,
                        "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    })
                }
            };
            if let Some(map) = out.content.as_object_mut() {
                map.insert(
                    "unknownFields".to_string(),
                    json!(unknown_field_names(
                        value,
                        &["type", "message", "uuid", "parentUuid", "sessionId", "timestamp",
                          "cwd", "version", "gitBranch", "isSidechain", "userType",
                          "permissionMode", "promptId", "entrypoint"],
                    )),
                );
            }
            out_records.push(Parsed::Record(out));
        }
        if out_records.is_empty() {
            out_records.push(Parsed::Ignored);
        }
        out_records
    }
}

fn base_record(
    native_id: &str,
    uuid: &str,
    seq: u64,
    timestamp: Option<String>,
    cwd: Option<String>,
) -> RecordOut {
    RecordOut {
        native_session_id: native_id.to_string(),
        native_event_id: Some(uuid.to_string()),
        source_key: format!("claude:{uuid}"),
        kind: "unknown".to_string(),
        seq,
        timestamp,
        content: json!({}),
        search_text: String::new(),
        cwd,
        extra_roots: Vec::new(),
        title_update: None,
    }
}

fn safe_token(value: &str) -> String {
    let filtered: String = value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':'))
        .take(80)
        .collect();
    if filtered.is_empty() {
        "unknown".to_string()
    } else {
        filtered
    }
}
