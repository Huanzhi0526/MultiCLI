//! CodeBuddy Code transcript parser.
//!
//! Observed format (CodeBuddy Code 2.179.0, `%USERPROFILE%\.codebuddy\projects\
//! <project-slug>\<session-uuid>.jsonl`): `session-meta` carries
//! `sessionId`/`id` and a millisecond `timestamp`; conversation rows are
//! `message` (role + block array), `reasoning`, `function_call` (`name`,
//! `arguments`, `callId`) and `function_call_result` (`output:{type,text}`,
//! `status`). `ai-title` and `summary` are metadata; `file-history-snapshot` is
//! editor state and is ignored.
//!
//! `%USERPROFILE%\.codebuddy\sessions\*.json` is **not** a transcript source:
//! those files are live runtime state (pid, heartbeat, endpoint). They are never
//! read here.

use super::{
    block_placeholder, content_text, fallback_event_id, message_text, native_title, search_body,
    string_field, title_from_text, tool_text, unknown_field_names, HistoryParser, LineError,
    ParseContext, Parsed, ParserCheckpoint, RecordOut, SessionMeta, TitleSource,
};
use crate::multicli::history::ids::{normalize_timestamp, stable_id};
use crate::multicli::history::model::{product_for_agent, HistoryProduct};
use serde_json::{json, Value};

#[derive(Default)]
pub struct CodeBuddyParser {
    native_id: Option<String>,
    cwd: Option<String>,
    title_seen: bool,
}

impl HistoryParser for CodeBuddyParser {
    fn product(&self) -> &'static HistoryProduct {
        product_for_agent("codebuddy").expect("codebuddy product is registered")
    }

    fn matches_first_line(&self, value: &Value) -> bool {
        value.get("type").and_then(Value::as_str) == Some("session-meta")
            && value.get("sessionId").and_then(Value::as_str).is_some()
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
            .unwrap_or_else(|| stable_id("codebuddy-file", &[ctx.file_stem]));
        let timestamp = value.get("timestamp").and_then(normalize_timestamp);
        let seq = ctx.line_index;
        let record_id = string_field(value, "id")
            .unwrap_or_else(|| fallback_event_id(record_type, ctx));

        match record_type {
            "session-meta" => {
                let started_at = timestamp.clone();
                Ok(vec![Parsed::Meta(SessionMeta {
                    native_id,
                    title: None,
                    title_source: TitleSource::None,
                    cwd: self.cwd.clone(),
                    extra_roots: Vec::new(),
                    started_at,
                    product_version: None,
                })])
            }
            "message" => {
                let role = string_field(value, "role").unwrap_or_else(|| "unknown".to_string());
                let (raw_text, blocks) =
                    content_text(value.get("content").unwrap_or(&Value::Null));
                let (text, truncated) = message_text(&raw_text);
                let mut out = base_record(&native_id, &record_id, seq, timestamp, self.cwd.clone());
                out.kind = match role.as_str() {
                    "user" => "user-message",
                    "assistant" => "assistant-message",
                    "system" => "system",
                    _ => "unknown",
                }
                .to_string();
                out.source_key = format!("buddy:message:{record_id}");
                out.native_event_id = Some(record_id.clone());
                out.content = json!({
                    "role": role,
                    "text": text,
                    "blocks": blocks,
                    "truncated": truncated || ctx.line_truncated,
                    "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    "unknownFields": unknown_field_names(
                        value,
                        &["type", "id", "role", "content", "sessionId", "cwd", "timestamp",
                          "parentId", "__codebuddyLocal", "_meta", "providerData"],
                    ),
                });
                out.search_text = search_body(&format!("{role} {text}"));
                if role == "user" && !self.title_seen {
                    if let Some(title) = title_from_text(&text) {
                        self.title_seen = true;
                        out.title_update = Some((title, TitleSource::FirstUserMessage));
                    }
                }
                Ok(vec![Parsed::Record(out)])
            }
            "reasoning" => {
                let raw = value
                    .get("content")
                    .map(|content| content_text(content).0)
                    .filter(|text| !text.trim().is_empty())
                    .or_else(|| {
                        value
                            .get("rawContent")
                            .map(|content| content_text(content).0)
                    })
                    .unwrap_or_default();
                let (text, truncated) = message_text(&raw);
                let mut out = base_record(&native_id, &record_id, seq, timestamp, self.cwd.clone());
                out.kind = "reasoning".to_string();
                out.source_key = format!("buddy:reasoning:{record_id}");
                out.native_event_id = Some(record_id.clone());
                out.content = json!({
                    "role": "assistant",
                    "text": if text.is_empty() { block_placeholder("reasoning") } else { text.clone() },
                    "blocks": ["reasoning"],
                    "truncated": truncated,
                    "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    "unknownFields": unknown_field_names(
                        value,
                        &["type", "id", "content", "rawContent", "sessionId", "cwd", "timestamp",
                          "parentId", "_meta", "providerData"],
                    ),
                });
                out.search_text = search_body(&text);
                Ok(vec![Parsed::Record(out)])
            }
            "function_call" => {
                let name = string_field(value, "name").unwrap_or_else(|| "tool".to_string());
                let arguments = string_field(value, "arguments").unwrap_or_default();
                let (arguments, truncated) = tool_text(&arguments, 65_536);
                let call_id = string_field(value, "callId").unwrap_or_else(|| record_id.clone());
                let mut out = base_record(&native_id, &record_id, seq, timestamp, self.cwd.clone());
                out.kind = "tool-call".to_string();
                out.source_key = format!("buddy:call:{record_id}");
                out.native_event_id = Some(record_id.clone());
                out.content = json!({
                    "role": "assistant",
                    "text": format!("[tool: {name}]"),
                    "tool": {
                        "name": safe_token(&name),
                        "callId": safe_token(&call_id),
                        "input": arguments,
                        "inputTruncated": truncated,
                    },
                    "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    "unknownFields": unknown_field_names(
                        value,
                        &["type", "id", "name", "arguments", "callId", "sessionId", "cwd",
                          "timestamp", "parentId", "_meta", "providerData"],
                    ),
                });
                out.search_text = search_body(&format!("{name} {arguments}"));
                Ok(vec![Parsed::Record(out)])
            }
            "function_call_result" => {
                let call_id = string_field(value, "callId").unwrap_or_default();
                let (raw_text, blocks) =
                    content_text(value.get("output").unwrap_or(&Value::Null));
                let (text, truncated) = message_text(&raw_text);
                let status = string_field(value, "status");
                let mut out = base_record(&native_id, &record_id, seq, timestamp, self.cwd.clone());
                out.kind = "tool-result".to_string();
                out.source_key = format!("buddy:result:{record_id}");
                out.native_event_id = Some(record_id.clone());
                out.content = json!({
                    "role": "tool",
                    "text": text,
                    "blocks": blocks,
                    "tool": {
                        "callId": safe_token(&call_id),
                        "name": string_field(value, "name").map(|name| safe_token(&name)),
                        "status": status.map(|status| safe_token(&status)),
                    },
                    "truncated": truncated || ctx.line_truncated,
                    "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    "unknownFields": unknown_field_names(
                        value,
                        &["type", "id", "name", "callId", "status", "output", "sessionId", "cwd",
                          "timestamp", "parentId", "_meta", "providerData"],
                    ),
                });
                out.search_text = search_body(&text);
                Ok(vec![Parsed::Record(out)])
            }
            "ai-title" => match native_title(&string_field(value, "aiTitle").unwrap_or_default()) {
                Some(title) => Ok(vec![Parsed::Title {
                    title,
                    source: TitleSource::NativeTitle,
                }]),
                None => Ok(vec![Parsed::Ignored]),
            },
            "summary" => {
                let text = string_field(value, "summary").unwrap_or_default();
                let (text, truncated) = message_text(&text);
                let mut out = base_record(&native_id, &record_id, seq, timestamp, self.cwd.clone());
                out.kind = "summary".to_string();
                out.source_key = format!("buddy:summary:{record_id}");
                out.native_event_id = Some(record_id.clone());
                out.content = json!({
                    "role": "system",
                    "text": text,
                    "truncated": truncated,
                    "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                });
                out.search_text = search_body(&text);
                Ok(vec![Parsed::Record(out)])
            }
            "file-history-snapshot" => Ok(vec![Parsed::Ignored]),
            other => {
                let mut out = base_record(&native_id, &record_id, seq, timestamp, self.cwd.clone());
                out.kind = "unknown".to_string();
                out.source_key = format!("buddy:unknown:{}", fallback_event_id(other, ctx));
                out.content = json!({
                    "recordType": safe_token(other),
                    "text": "",
                    "unknownFields": unknown_field_names(value, &["type"]),
                });
                Ok(vec![Parsed::Record(out)])
            }
        }
    }
}

fn base_record(
    native_id: &str,
    record_id: &str,
    seq: u64,
    timestamp: Option<String>,
    cwd: Option<String>,
) -> RecordOut {
    RecordOut {
        native_session_id: native_id.to_string(),
        native_event_id: Some(record_id.to_string()),
        source_key: format!("buddy:{record_id}"),
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
