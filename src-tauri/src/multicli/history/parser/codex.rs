//! Codex CLI rollout parser.
//!
//! Observed format (Codex CLI 0.166.0, `%USERPROFILE%\.codex\sessions\
//! YYYY\MM\DD\rollout-<ts>-<uuid>.jsonl[.zst]`): one JSON object per line with
//! `{ ordinal, timestamp, type, payload }`. `session_meta` carries
//! `payload.session_id`, `cwd`, `cli_version` and `runtime_workspace_roots`;
//! conversation rows arrive as `response_item` (`message`, `function_call`,
//! `function_call_output`, `custom_tool_call`, `reasoning`), `event_msg`
//! (`user_message`, `agent_message`, task markers) or `compacted`.
//!
//! Everything else in the rollout (world state, turn context, token accounting,
//! encrypted reasoning) is either ignored or reduced to its type name.

use super::{
    block_placeholder, content_text, fallback_event_id, message_text, search_body, string_field,
    string_list, title_from_text, tool_text, unknown_field_names, HistoryParser, LineError,
    ParseContext, Parsed, ParserCheckpoint, RecordOut, SessionMeta, TitleSource,
};
use crate::multicli::history::model::{product_for_agent, HistoryProduct};
use crate::multicli::history::ids::{normalize_timestamp, stable_id};
use serde_json::{json, Value};

#[derive(Default)]
pub struct CodexParser {
    native_id: Option<String>,
    cwd: Option<String>,
    extra_roots: Vec<String>,
    title_seen: bool,
}

impl HistoryParser for CodexParser {
    fn product(&self) -> &'static HistoryProduct {
        product_for_agent("codex").expect("codex product is registered")
    }

    fn matches_first_line(&self, value: &Value) -> bool {
        value.get("type").and_then(Value::as_str) == Some("session_meta")
            && value
                .get("payload")
                .and_then(|payload| payload.get("session_id").or_else(|| payload.get("id")))
                .and_then(Value::as_str)
                .is_some()
    }

    fn checkpoint(&self) -> ParserCheckpoint {
        ParserCheckpoint {
            native_session_id: self.native_id.clone(),
            cwd: self.cwd.clone(),
            extra_roots: self.extra_roots.clone(),
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
        if self.extra_roots.is_empty() {
            self.extra_roots = checkpoint.extra_roots.clone();
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
        let payload = value.get("payload").cloned().unwrap_or(Value::Null);

        if record_type == "session_meta" {
            return Ok(self.session_meta(&payload, ctx));
        }

        let native_id = self
            .native_id
            .clone()
            .unwrap_or_else(|| native_id_from_stem(ctx.file_stem));

        let seq = value
            .get("ordinal")
            .and_then(Value::as_u64)
            .unwrap_or(ctx.line_index);
        let timestamp = value.get("timestamp").and_then(normalize_timestamp);

        let mut out = RecordOut {
            native_session_id: native_id.clone(),
            native_event_id: None,
            source_key: String::new(),
            kind: "unknown".to_string(),
            seq,
            timestamp,
            content: json!({}),
            search_text: String::new(),
            cwd: self.cwd.clone(),
            extra_roots: Vec::new(),
            title_update: None,
        };

        let parsed = match record_type {
            "response_item" => self.response_item(&payload, ctx, &mut out)?,
            "event_msg" => self.event_msg(&payload, ctx, &mut out)?,
            "compacted" | "compaction" => {
                let text = string_field(&payload, "message").unwrap_or_default();
                self.simple_record("summary", "system", &text, &payload, ctx, &mut out)?
            }
            "world_state" | "turn_context" | "token_usage_record" => {
                return Ok(vec![Parsed::Ignored]);
            }
            other => {
                out.kind = "unknown".to_string();
                out.content = json!({
                    "recordType": safe_name(other),
                    "unknownFields": unknown_field_names(&payload, &[]),
                    "text": "",
                });
                out.source_key = format!("codex:record:{}", fallback_event_id(other, ctx));
                return Ok(vec![Parsed::Record(out)]);
            }
        };
        Ok(parsed)
    }
}

impl CodexParser {
    fn session_meta(&mut self, payload: &Value, ctx: &ParseContext<'_>) -> Vec<Parsed> {
        let native_id = string_field(payload, "session_id")
            .or_else(|| string_field(payload, "id"))
            .unwrap_or_else(|| native_id_from_stem(ctx.file_stem));
        self.native_id = Some(native_id.clone());
        self.cwd = string_field(payload, "cwd");
        self.extra_roots = string_list(payload, "runtime_workspace_roots");
        let started_at = payload.get("timestamp").and_then(normalize_timestamp);
        vec![Parsed::Meta(SessionMeta {
            native_id,
            title: None,
            title_source: TitleSource::None,
            cwd: self.cwd.clone(),
            extra_roots: self.extra_roots.clone(),
            started_at,
            product_version: string_field(payload, "cli_version"),
        })]
    }

    fn response_item(
        &mut self,
        payload: &Value,
        ctx: &ParseContext<'_>,
        out: &mut RecordOut,
    ) -> Result<Vec<Parsed>, LineError> {
        let item_type = payload
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| LineError::new("missing-payload-type", "response_item has no type"))?;
        let event_id = string_field(payload, "id")
            .or_else(|| string_field(payload, "call_id"))
            .unwrap_or_else(|| fallback_event_id(item_type, ctx));
        out.native_event_id = Some(event_id.clone());
        out.source_key = format!("codex:response:{event_id}");

        match item_type {
            "message" => {
                let role = string_field(payload, "role").unwrap_or_else(|| "unknown".to_string());
                let (raw_text, blocks) =
                    content_text(payload.get("content").unwrap_or(&Value::Null));
                let (text, truncated) = message_text(&raw_text);
                out.kind = match role.as_str() {
                    "user" => "user-message",
                    "assistant" => "assistant-message",
                    "system" | "developer" => "system",
                    _ => "unknown",
                }
                .to_string();
                out.content = json!({
                    "role": role,
                    "text": text,
                    "blocks": blocks,
                    "truncated": truncated || ctx.line_truncated,
                    "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    "unknownFields": unknown_field_names(
                        payload,
                        &["type", "id", "role", "content", "internal_chat_message_metadata_passthrough"],
                    ),
                });
                out.search_text = search_body(&format!("{role} {text}"));
                if role == "user" && !self.title_seen {
                    if let Some(title) = title_from_text(&text) {
                        self.title_seen = true;
                        out.title_update = Some((title, TitleSource::FirstUserMessage));
                    }
                }
            }
            "function_call" | "custom_tool_call" | "local_shell_call" => {
                let name = string_field(payload, "name")
                    .or_else(|| string_field(payload, "type"))
                    .unwrap_or_else(|| "tool".to_string());
                let input = string_field(payload, "arguments")
                    .or_else(|| string_field(payload, "input"))
                    .unwrap_or_default();
                let (text, truncated) = tool_text(&input, 65_536);
                let call_id = string_field(payload, "call_id").unwrap_or_else(|| event_id.clone());
                out.kind = "tool-call".to_string();
                out.content = json!({
                    "role": "assistant",
                    "text": format!("[tool: {name}]"),
                    "tool": {
                        "name": safe_name(&name),
                        "callId": safe_name(&call_id),
                        "input": text,
                        "inputTruncated": truncated,
                    },
                    "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    "unknownFields": unknown_field_names(
                        payload,
                        &["type", "id", "name", "arguments", "input", "call_id", "status",
                          "internal_chat_message_metadata_passthrough"],
                    ),
                });
                out.search_text = search_body(&format!("{name} {input}"));
            }
            "function_call_output" | "custom_tool_call_output" => {
                let call_id = string_field(payload, "call_id").unwrap_or_else(|| event_id.clone());
                let output = payload.get("output").cloned().unwrap_or(Value::Null);
                let (raw_text, blocks) = content_text(&output);
                let (text, truncated) = message_text(&raw_text);
                out.kind = "tool-result".to_string();
                out.content = json!({
                    "role": "tool",
                    "text": text,
                    "blocks": blocks,
                    "tool": { "callId": safe_name(&call_id) },
                    "truncated": truncated || ctx.line_truncated,
                    "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    "unknownFields": unknown_field_names(
                        payload,
                        &["type", "id", "call_id", "output",
                          "internal_chat_message_metadata_passthrough"],
                    ),
                });
                out.search_text = search_body(&text);
            }
            "reasoning" => {
                let summary = payload
                    .get("summary")
                    .map(|value| content_text(value).0)
                    .unwrap_or_default();
                let (text, truncated) = message_text(&summary);
                out.kind = "reasoning".to_string();
                out.content = json!({
                    "role": "assistant",
                    "text": if text.is_empty() { block_placeholder("reasoning") } else { text.clone() },
                    "blocks": ["reasoning"],
                    "encrypted": payload.get("encrypted_content").is_some(),
                    "truncated": truncated,
                    "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
                    "unknownFields": unknown_field_names(
                        payload,
                        &["type", "id", "summary", "encrypted_content",
                          "internal_chat_message_metadata_passthrough"],
                    ),
                });
                out.search_text = search_body(&text);
            }
            other => {
                out.kind = "unknown".to_string();
                let (raw_text, _) = content_text(payload.get("content").unwrap_or(&Value::Null));
                let (text, _) = message_text(&raw_text);
                out.content = json!({
                    "recordType": safe_name(other),
                    "text": text,
                    "unknownFields": unknown_field_names(
                        payload,
                        &["type", "id", "content", "name", "arguments", "call_id", "output"],
                    ),
                });
                out.search_text = search_body(&text);
            }
        }
        Ok(vec![Parsed::Record(out.clone())])
    }

    fn event_msg(
        &mut self,
        payload: &Value,
        ctx: &ParseContext<'_>,
        out: &mut RecordOut,
    ) -> Result<Vec<Parsed>, LineError> {
        let event_type = payload
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| LineError::new("missing-payload-type", "event_msg has no type"))?;
        match event_type {
            "user_message" | "agent_message" => {
                let text = string_field(payload, "message").unwrap_or_default();
                let role = if event_type == "user_message" { "user" } else { "assistant" };
                let native = format!("event:{event_type}:{}", fallback_event_id(event_type, ctx));
                self.simple_record(
                    if role == "user" { "user-message" } else { "assistant-message" },
                    role,
                    &text,
                    payload,
                    ctx,
                    out,
                )
                .map(|mut parsed| {
                    if let Some(Parsed::Record(record)) = parsed.first_mut() {
                        record.native_event_id = Some(native.clone());
                        record.source_key = format!("codex:event:{native}");
                    }
                    parsed
                })
            }
            "turn_aborted" | "error" | "stream_error" | "context_compacted" => {
                let text = string_field(payload, "message")
                    .or_else(|| string_field(payload, "reason"))
                    .unwrap_or_default();
                self.simple_record("system", "system", &text, payload, ctx, out)
            }
            "token_count" | "thread_settings_applied" | "task_started" | "task_complete"
            | "item_completed" | "item_started" => {
                // Task markers are not chat messages, but they are not errors
                // either: they are understood and intentionally not shown.
                Ok(vec![Parsed::Ignored])
            }
            other => {
                out.kind = "unknown".to_string();
                out.source_key = format!("codex:event:{}", fallback_event_id(other, ctx));
                out.content = json!({
                    "recordType": safe_name(other),
                    "text": "",
                    "unknownFields": unknown_field_names(payload, &["type"]),
                });
                Ok(vec![Parsed::Record(out.clone())])
            }
        }
    }

    fn simple_record(
        &mut self,
        kind: &str,
        role: &str,
        raw_text: &str,
        payload: &Value,
        ctx: &ParseContext<'_>,
        out: &mut RecordOut,
    ) -> Result<Vec<Parsed>, LineError> {
        let (text, truncated) = message_text(raw_text);
        out.kind = kind.to_string();
        if out.native_event_id.is_none() {
            let fallback = fallback_event_id(kind, ctx);
            out.native_event_id = Some(fallback.clone());
            out.source_key = format!("codex:{kind}:{fallback}");
        }
        out.content = json!({
            "role": role,
            "text": text,
            "blocks": ["text"],
            "truncated": truncated || ctx.line_truncated,
            "timeSource": if out.timestamp.is_some() { "native" } else { "file-mtime" },
            "unknownFields": unknown_field_names(payload, &["type", "message", "text"]),
        });
        out.search_text = search_body(&text);
        Ok(vec![Parsed::Record(out.clone())])
    }
}

/// Codex names rollout files `rollout-<timestamp>-<uuid>.jsonl[.zst]`. The
/// trailing five dash-separated groups are the session uuid; using it keeps a
/// session id stable even if the first line is unreadable.
fn native_id_from_stem(stem: &str) -> String {
    let stem = stem.strip_suffix(".jsonl").unwrap_or(stem);
    let stem = stem.strip_suffix(".jsonl.zst").unwrap_or(stem);
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() >= 5 {
        let tail = parts[parts.len() - 5..].join("-");
        if tail.len() == 36 && tail.chars().filter(|ch| *ch == '-').count() == 4 {
            return tail;
        }
    }
    stable_id("codex-file", &[stem])
}

fn safe_name(value: &str) -> String {
    let filtered: String = value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':' | '/' | ' '))
        .take(80)
        .collect();
    if filtered.trim().is_empty() {
        "unknown".to_string()
    } else {
        filtered
    }
}
