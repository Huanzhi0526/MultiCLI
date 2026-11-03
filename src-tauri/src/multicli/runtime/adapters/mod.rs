//! Protocol adapters: one module per product, one shared transport.
//!
//! An adapter only translates. It never decides product policy:
//!
//! - it never answers a permission request on its own;
//! - it never turns a missing capability into a fake success;
//! - it never parses terminal bytes: every product here has a documented
//!   structured surface, and that is the only thing read.
//!
//! What is shared lives here: the sink type, the JSON-RPC peer used by Codex
//! (`codex app-server`) and CodeBuddy (ACP), and the small JSON helpers the
//! three adapters need.

pub mod claude;
pub mod codebuddy;
pub mod codex;

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::multicli::error::{AppError, ErrorCode};

use super::process::OwnedProcess;
use super::run::{AdapterEvent, EventMeta};

pub type SinkFn = Arc<dyn Fn(AdapterEvent, EventMeta) + Send + Sync>;

/// Sends one event with its dedupe metadata.
pub struct Emitter {
    sink: SinkFn,
}

impl Clone for Emitter {
    fn clone(&self) -> Self {
        Self {
            sink: Arc::clone(&self.sink),
        }
    }
}

impl Emitter {
    pub fn new(sink: SinkFn) -> Self {
        Self { sink }
    }

    pub fn emit(&self, event: AdapterEvent, meta: EventMeta) {
        (self.sink)(event, meta);
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC peer
// ---------------------------------------------------------------------------

type ResponseSlot = Sender<Result<Value, RpcFailure>>;

#[derive(Debug, Clone)]
pub struct RpcFailure {
    pub code: i64,
    pub message: String,
}

impl RpcFailure {
    pub fn to_error(&self, method: &str) -> AppError {
        AppError::new(
            ErrorCode::Protocol,
            format!("{method} was refused by the CLI: {}", self.message),
        )
        .with_details(json!({ "method": method, "rpcCode": self.code }))
    }
}

/// Minimal JSON-RPC 2.0 client over a line-delimited stdio pipe.
///
/// Requests are matched by the id this peer allocated, never by arrival order:
/// a server-initiated request (an approval prompt) arriving between a request
/// and its reply must not be mistaken for the reply.
pub struct RpcPeer {
    process: Arc<OwnedProcess>,
    next_id: Mutex<i64>,
    pending: Mutex<HashMap<i64, ResponseSlot>>,
    /// Server → client requests awaiting an answer, keyed by the id rendered as
    /// a string so a `u64` id and a `"abc"` id can coexist.
    server_requests: Mutex<HashMap<String, Value>>,
}

impl RpcPeer {
    pub fn new(process: Arc<OwnedProcess>) -> Self {
        Self {
            process,
            next_id: Mutex::new(0),
            pending: Mutex::new(HashMap::new()),
            server_requests: Mutex::new(HashMap::new()),
        }
    }

    /// Allocates an id, registers a waiter and writes the request. The
    /// returned receiver yields exactly one reply.
    fn begin(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(i64, Receiver<Result<Value, RpcFailure>>), AppError> {
        let id = {
            let mut next = self.next_id.lock();
            *next += 1;
            *next
        };
        let (sender, receiver) = channel();
        self.pending.lock().insert(id, sender);
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(error) = self.write(&frame) {
            self.pending.lock().remove(&id);
            return Err(error);
        }
        Ok((id, receiver))
    }

    /// Sends a request and waits for its reply.
    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, AppError> {
        let (id, receiver) = self.begin(method, params)?;
        match receiver.recv_timeout(timeout) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(failure)) => Err(failure.to_error(method)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.pending.lock().remove(&id);
                Err(AppError::new(
                    ErrorCode::Timeout,
                    format!("{method} did not answer within {} ms", timeout.as_millis()),
                )
                .with_details(json!({ "method": method, "stage": "rpcTimeout" }))
                .with_retryable(true))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(AppError::new(
                ErrorCode::Protocol,
                format!("the CLI ended before {method} was answered"),
            )
            .with_details(json!({ "method": method, "stage": "rpcDisconnected" }))),
        }
    }

    /// Allocates an id and writes the request **without** waiting for the
    /// reply. Used for calls whose answer marks the end of a turn
    /// (`session/prompt`): waiting there would block the IPC command for the
    /// whole turn.
    pub fn send_async(&self, method: &str, params: Value) -> Result<i64, AppError> {
        let id = {
            let mut next = self.next_id.lock();
            *next += 1;
            *next
        };
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        Ok(id)
    }

    /// Register routing before bytes become visible to a fast peer.
    pub fn send_async_registered(
        &self,
        method: &str,
        params: Value,
        register: impl FnOnce(i64),
    ) -> Result<i64, AppError> {
        let id = {
            let mut next = self.next_id.lock();
            *next += 1;
            *next
        };
        register(id);
        self.write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        Ok(id)
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), AppError> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    /// A notification whose method carries no params at all. Codex's
    /// `initialized` is exactly this shape.
    pub fn notify_bare(&self, method: &str) -> Result<(), AppError> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method }))
    }

    pub fn respond(&self, id: &Value, result: Value) -> Result<(), AppError> {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    pub fn respond_error(&self, id: &Value, code: i64, message: &str) -> Result<(), AppError> {
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }))
    }

    fn write(&self, frame: &Value) -> Result<(), AppError> {
        let text = serde_json::to_string(frame).map_err(|error| {
            AppError::new(
                ErrorCode::Protocol,
                format!("failed to encode a protocol frame: {error}"),
            )
        })?;
        self.process.write_line(&text)
    }

    /// Registers a server request so a later `answer` can reply to the exact id.
    pub fn park_server_request(&self, id: &Value) -> String {
        let key = id_key(id);
        self.server_requests.lock().insert(key.clone(), id.clone());
        key
    }

    pub fn take_server_request(&self, key: &str) -> Option<Value> {
        self.server_requests.lock().remove(key)
    }

    /// Puts a server request id back after a failed write, so a retry can still
    /// answer the original request instead of inventing a new one.
    pub fn repark_server_request(&self, key: &str, id: Value) {
        self.server_requests.lock().insert(key.to_string(), id);
    }

    /// Called by the reader when a response arrives. Returns `true` when the id
    /// belonged to one of our requests.
    pub fn complete(&self, id: i64, result: Result<Value, RpcFailure>) -> bool {
        let sender = self.pending.lock().remove(&id);
        match sender {
            Some(sender) => {
                let _ = sender.send(result);
                true
            }
            None => false,
        }
    }

    /// The reader thread ended: every waiter must be released instead of
    /// blocking until its timeout.
    pub fn fail_all(&self, reason: &str) {
        let drained: Vec<(i64, ResponseSlot)> = {
            let mut pending = self.pending.lock();
            let keys: Vec<i64> = pending.keys().copied().collect();
            keys.into_iter()
                .filter_map(|id| pending.remove(&id).map(|sender| (id, sender)))
                .collect()
        };
        for (_, sender) in drained {
            let _ = sender.send(Err(RpcFailure {
                code: -32000,
                message: reason.to_string(),
            }));
        }
    }

    pub fn pending_server_requests(&self) -> usize {
        self.server_requests.lock().len()
    }
}

/// Renders a JSON-RPC id as a string key. Numbers keep their decimal form so
/// `1` and `"1"` cannot collide in a way that matters for a single product.
pub fn id_key(id: &Value) -> String {
    match id {
        Value::String(text) => format!("s:{text}"),
        other => format!("n:{other}"),
    }
}

// ---------------------------------------------------------------------------
// Shared JSON helpers
// ---------------------------------------------------------------------------

pub fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().map(str::to_string)
}

pub fn text_of_blocks(value: &Value) -> String {
    let Some(items) = value.as_array() else {
        return String::new();
    };
    let mut parts = Vec::new();
    for item in items {
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            parts.push(text.to_string());
        }
    }
    parts.join("")
}

/// One-line preview of a tool input, bounded, so a report never contains a
/// megabyte of patch text.
pub fn preview(value: &Value, limit: usize) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    let mut out: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        out.push('…');
    }
    out
}

/// Converts a protocol status word into the runtime's tool status vocabulary.
pub fn tool_status(raw: &str) -> String {
    match raw {
        "inProgress" | "in_progress" | "running" | "pending" | "started" => "running".into(),
        "completed" | "success" | "succeeded" | "ok" => "completed".into(),
        "failed" | "error" | "errored" => "failed".into(),
        "declined" | "denied" | "rejected" => "denied".into(),
        "cancelled" | "canceled" | "interrupted" | "aborted" => "cancelled".into(),
        other => other.to_string(),
    }
}

/// Splits a content list into (text, thinking) while ignoring everything else.
pub fn split_content_blocks(value: &Value) -> (String, String) {
    let mut text = String::new();
    let mut thinking = String::new();
    let Some(items) = value.as_array() else {
        return (text, thinking);
    };
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(value) = item.get("text").and_then(Value::as_str) {
                    text.push_str(value);
                }
            }
            Some("thinking") => {
                if let Some(value) = item.get("thinking").and_then(Value::as_str) {
                    thinking.push_str(value);
                }
            }
            _ => {}
        }
    }
    (text, thinking)
}

/// A process-wide monotonic counter used only to build a dedupe key for an
/// event that carries no native id of its own. It is never displayed and never
/// used as an ordering guarantee.
pub fn next_local() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// `prefix#<n>`, stable for one emitted event and unique within the process.
pub fn local_key(prefix: &str) -> String {
    format!("{prefix}#{}", next_local())
}

/// A reader-thread helper: parses one line, and reports a malformed line as a
/// warning instead of killing the stream. A protocol that cannot be parsed at
/// all is a real error only when the whole connection is unusable.
pub fn parse_line(line: &str) -> Result<Value, String> {
    serde_json::from_str::<Value>(line)
        .map_err(|error| format!("{error}: {}", preview_str(line, 200)))
}

fn preview_str(value: &str, limit: usize) -> String {
    let mut out: String = value.chars().take(limit).collect();
    if value.chars().count() > limit {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_status_normalises_both_spellings() {
        assert_eq!(tool_status("inProgress"), "running");
        assert_eq!(tool_status("completed"), "completed");
        assert_eq!(tool_status("declined"), "denied");
        assert_eq!(tool_status("interrupted"), "cancelled");
        assert_eq!(tool_status("something-else"), "something-else");
    }

    #[test]
    fn id_keys_do_not_collide_across_types() {
        assert_eq!(id_key(&json!(1)), "n:1");
        assert_eq!(id_key(&json!("1")), "s:1");
        assert_ne!(id_key(&json!(1)), id_key(&json!("1")));
    }

    #[test]
    fn preview_truncates_on_a_character_boundary() {
        assert_eq!(preview(&json!({"a": 1}), 100), "{\"a\":1}");
        let text = preview(&json!("你好世界"), 2);
        assert_eq!(text, "你好…");
    }

    #[test]
    fn content_blocks_split_text_and_thinking() {
        let value = json!([
            {"type": "thinking", "thinking": "reason"},
            {"type": "text", "text": "answer"},
            {"type": "tool_use", "id": "t1"},
        ]);
        let (text, thinking) = split_content_blocks(&value);
        assert_eq!(text, "answer");
        assert_eq!(thinking, "reason");
    }

    /// A failed wire write must leave the *same* JSON-RPC id parked, otherwise
    /// the retry has a restored pending entry but nothing to answer. This is
    /// the production `RpcPeer` ledger, not a fake adapter.
    #[test]
    fn a_failed_write_restores_the_exact_wire_id_and_never_duplicates_it() {
        let peer = RpcPeer::new(Arc::new(OwnedProcess::detached_for_tests()));
        // The parked value is the JSON-RPC id itself, which is what the reply
        // frame echoes.
        let wire_id = json!(7);
        let key = peer.park_server_request(&wire_id);
        let taken = peer.take_server_request(&key).expect("parked");
        assert!(peer.take_server_request(&key).is_none());

        // The detached process has no stdin, so the write fails exactly like a
        // broken pipe would.
        assert!(
            peer.respond(
                &taken,
                json!({ "outcome": { "outcome": "selected", "optionId": "allow" } })
            )
            .is_err(),
            "a detached process must reject writes"
        );
        peer.repark_server_request(&key, taken);

        let retry = peer
            .take_server_request(&key)
            .expect("the retry can answer");
        assert_eq!(
            retry,
            json!(7),
            "the retry must answer the original id, not a new one"
        );
        assert!(
            peer.take_server_request(&key).is_none(),
            "one parked request must not yield two answers"
        );
    }
}
