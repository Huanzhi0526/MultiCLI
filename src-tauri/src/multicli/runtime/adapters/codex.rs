//! Codex CLI adapter — `codex app-server` JSON-RPC.
//!
//! Verified against the installed `codex-cli 0.166.0` schema
//! (`scratch/s00/protocol-profiles/codex-schema-0.166.0`, re-read for this
//! stage) and against a live handshake.
//!
//! The only thing this adapter deliberately does **not** do is inherit the old
//! engine's `approvalPolicy: "never"`. That policy turns every permission
//! prompt into a silent refusal-or-allow on Codex's side, which is exactly the
//! behaviour S05 replaces: the thread is opened with `on-request`, and the
//! server's own approval requests become real UI cards.
//!
//! What the server sends (0.166.0):
//!
//! | direction | method | payload |
//! |---|---|---|
//! | client → server | `initialize` | `{clientInfo, capabilities}` |
//! | client → server | `initialized` | — |
//! | client → server | `thread/start` / `thread/resume` | `{cwd, approvalPolicy, sandbox, model?}` |
//! | client → server | `turn/start` | `{threadId, input:[{type:"text",text}]}` |
//! | client → server | `turn/interrupt` | `{threadId, turnId}` |
//! | server → client | `item/commandExecution/requestApproval` | `{itemId, turnId, command, cwd, ...}` |
//! | server → client | `item/fileChange/requestApproval` | `{itemId, turnId, reason, ...}` |
//! | server → client | `item/agentMessage/delta` | `{itemId, delta, turnId}` |
//! | server → client | `item/started` / `item/completed` | `{item:{type,...}}` |
//! | server → client | `turn/started` / `turn/completed` | `{turn:{id,status}}` |
//!
//! Approval replies are `{"decision": "accept"|"acceptForSession"|"decline"|
//! "cancel"}`, which is why a denied write is a real denial the CLI understands
//! rather than a dropped request.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::multicli::error::{AppError, ErrorCode};

use super::super::model::RuntimePermissionOption;
use super::super::process::{LineReader, OwnedProcess};
use super::super::run::{Adapter, AdapterEvent, EventMeta};
use super::{id_key, local_key, parse_line, preview, tool_status, Emitter, RpcPeer};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
const TURN_ACK_TIMEOUT: Duration = Duration::from_secs(60);

/// Methods that are known, expected and deliberately not shown as events. They
/// are still counted: an unexpected one is visible in the run notes.
const NOISE: &[&str] = &[
    "thread/started",
    "thread/status/changed",
    "thread/readState/changed",
    "thread/name/updated",
    "thread/tokenUsage/updated",
    "thread/settings/updated",
    "thread/environment/connected",
    "thread/environment/disconnected",
    "turn/diff/updated",
    "turn/plan/updated",
    // Mid-flight tool output. `item/completed` carries the full
    // `aggregatedOutput`, so buffering the deltas would duplicate it; the
    // completed snapshot is applied as an upsert instead.
    "item/commandExecution/outputDelta",
    "item/fileChange/outputDelta",
    "process/outputDelta",
    "command/exec/outputDelta",
    "mcpServer/startupStatus/updated",
    "remoteControl/status/changed",
    "account/updated",
    "account/rateLimits/updated",
    "fs/changed",
    "skills/changed",
    "project/changed",
    "app/list/updated",
    "model/rerouted",
    "turn/moderationMetadata",
];

pub struct CodexAdapter {
    peer: Arc<RpcPeer>,
    process: Arc<OwnedProcess>,
    emitter: Emitter,
    thread_id: Mutex<Option<String>>,
    turn_id: Mutex<Option<String>>,
    delta_index: Mutex<HashMap<String, usize>>,
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
    closing: AtomicBool,
}

pub struct CodexOptions {
    pub cwd: String,
    pub sandbox: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// `Some(thread_id)` continues that conversation in a new Run.
    pub resume_thread: Option<String>,
}

impl CodexAdapter {
    pub fn connect(
        process: Arc<OwnedProcess>,
        stdout: std::process::ChildStdout,
        emitter: Emitter,
        options: CodexOptions,
    ) -> Result<Arc<Self>, AppError> {
        let peer = Arc::new(RpcPeer::new(Arc::clone(&process)));
        let adapter = Arc::new(Self {
            peer: Arc::clone(&peer),
            process,
            emitter,
            thread_id: Mutex::new(None),
            turn_id: Mutex::new(None),
            delta_index: Mutex::new(HashMap::new()),
            reader: Mutex::new(None),
            closing: AtomicBool::new(false),
        });

        // The reader has to run before the handshake: `initialize` is answered
        // on this same stdout.
        let reader_emitter = adapter.emitter.clone();
        let reader_peer = Arc::clone(&peer);
        let reader_thread_id = Arc::new(Mutex::new(None::<String>));
        let shared_thread = Arc::clone(&reader_thread_id);
        let handle = std::thread::Builder::new()
            .name("multicli-codex-reader".into())
            .spawn(move || {
                read_loop(stdout, reader_emitter, reader_peer, shared_thread);
            })
            .map_err(|error| {
                AppError::new(
                    ErrorCode::Io,
                    format!("failed to start the Codex reader thread: {error}"),
                )
                .with_details(json!({ "stage": "spawnReader" }))
            })?;
        *adapter.reader.lock() = Some(handle);

        adapter.handshake(&options, &reader_thread_id)?;
        Ok(adapter)
    }

    fn handshake(
        &self,
        options: &CodexOptions,
        reader_thread_id: &Arc<Mutex<Option<String>>>,
    ) -> Result<(), AppError> {
        self.peer.call(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "multicli",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": { "experimentalApi": true },
            }),
            HANDSHAKE_TIMEOUT,
        )?;
        self.peer.notify_bare("initialized")?;

        let mut params = json!({
            "cwd": options.cwd,
            // The product never runs with approvals disabled: the server's own
            // request becomes the UI card.
            "approvalPolicy": "on-request",
            "sandbox": options.sandbox,
        });
        if let Some(model) = &options.model {
            params["model"] = json!(model);
        }
        if let Some(effort) = &options.effort {
            if let Some(object) = params.as_object_mut() {
                object.insert("config".into(), json!({ "model_reasoning_effort": effort }));
            }
        }
        let (method, params) = match &options.resume_thread {
            Some(thread_id) => (
                "thread/resume",
                json!({
                    "threadId": thread_id,
                    "cwd": options.cwd,
                    "approvalPolicy": "on-request",
                    "sandbox": options.sandbox,
                }),
            ),
            None => ("thread/start", params),
        };
        let reply = self.peer.call(method, params, HANDSHAKE_TIMEOUT)?;
        let thread_id = reply
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| options.resume_thread.clone())
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::Protocol,
                    format!("{method} returned no thread id"),
                )
                .with_details(json!({ "stage": "handshake", "method": method }))
            })?;
        *self.thread_id.lock() = Some(thread_id.clone());
        *reader_thread_id.lock() = Some(thread_id.clone());
        self.emitter.emit(
            AdapterEvent::SessionBound {
                native_session_id: thread_id,
            },
            EventMeta::key("codex/session-bound"),
        );
        Ok(())
    }

    pub fn thread_id(&self) -> Option<String> {
        self.thread_id.lock().clone()
    }

    fn ensure_thread(&self) -> Result<String, AppError> {
        self.thread_id.lock().clone().ok_or_else(|| {
            AppError::conflict("run", "the Codex thread is not open yet")
                .with_details(json!({ "stage": "codexThread" }))
        })
    }
}

impl Adapter for CodexAdapter {
    fn native_session_id(&self) -> Option<String> {
        self.thread_id()
    }

    fn send_prompt(&self, text: &str) -> Result<(), AppError> {
        let thread_id = self.ensure_thread()?;
        let reply = self.peer.call(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{ "type": "text", "text": text }],
            }),
            TURN_ACK_TIMEOUT,
        )?;
        if let Some(turn_id) = reply
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            *self.turn_id.lock() = Some(turn_id.clone());
            self.emitter.emit(
                AdapterEvent::TurnStarted {
                    turn_id: turn_id.clone(),
                    model: reply
                        .pointer("/turn/model")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    effort: None,
                },
                // The id is part of the key: a constant key would make the
                // second turn's start look like a re-delivery of the first and
                // drop it before the state machine ever saw it.
                EventMeta::key(format!("codex/turn-start-ack:{turn_id}")),
            );
        }
        Ok(())
    }

    fn cancel_turn(&self, reason: &str) -> Result<(), AppError> {
        let thread_id = self.ensure_thread()?;
        let turn_id = self.turn_id.lock().clone().ok_or_else(|| {
            AppError::conflict("run", "there is no Codex turn to interrupt")
                .with_details(json!({ "stage": "codexInterrupt" }))
        })?;
        self.peer.call(
            "turn/interrupt",
            json!({ "threadId": thread_id, "turnId": turn_id }),
            Duration::from_secs(30),
        )?;
        self.emitter.emit(
            AdapterEvent::Warning {
                code: "cancelRequested".into(),
                message: format!("已请求 Codex 中断当前 turn：{reason}"),
            },
            EventMeta::key(local_key("codex/cancel")),
        );
        Ok(())
    }

    fn answer_permission(
        &self,
        native_request_id: &str,
        option_id: &str,
        _kind: &str,
    ) -> Result<(), AppError> {
        let raw = self.peer.take_server_request(native_request_id).ok_or_else(|| {
            AppError::conflict(
                "permission",
                "this Codex request is no longer waiting for an answer",
            )
            .with_details(json!({ "stage": "codexApproval", "requestId": native_request_id }))
        })?;
        self.peer.respond(&raw, json!({ "decision": option_id }))
    }

    fn shutdown(&self) {
        if self.closing.swap(true, Ordering::SeqCst) {
            return;
        }
        self.process.close_stdin();
        if let Some(handle) = self.reader.lock().take() {
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

fn read_loop(
    stdout: std::process::ChildStdout,
    emitter: Emitter,
    peer: Arc<RpcPeer>,
    thread_id: Arc<Mutex<Option<String>>>,
) {
    let mut reader = LineReader::new(stdout);
    loop {
        match reader.next_line() {
            Ok(None) => break,
            Ok(Some(line)) => {
                let value = match parse_line(&line) {
                    Ok(value) => value,
                    Err(error) => {
                        emitter.emit(
                            AdapterEvent::Warning {
                                code: "unparsableLine".into(),
                                message: error,
                            },
                            EventMeta::key("codex/unparsable"),
                        );
                        continue;
                    }
                };
                dispatch(&emitter, &peer, &thread_id, value);
            }
            Err(error) => {
                emitter.emit(
                    AdapterEvent::ProtocolError {
                        message: error.message,
                    },
                    EventMeta::key("codex/read-error"),
                );
                break;
            }
        }
    }
    peer.fail_all("the Codex app-server ended");
    emitter.emit(
        AdapterEvent::Exited {
            code: None,
            reason: Some("Codex app-server 的输出流已结束".into()),
        },
        EventMeta::key("codex/eof"),
    );
}

fn dispatch(
    emitter: &Emitter,
    peer: &Arc<RpcPeer>,
    thread_id: &Arc<Mutex<Option<String>>>,
    value: Value,
) {
    if let Some(method) = value.get("method").and_then(Value::as_str) {
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        match value.get("id") {
            Some(id) => handle_server_request(emitter, peer, id, method, params),
            None => handle_notification(emitter, thread_id, method, params),
        }
        return;
    }
    if let Some(id) = value.get("id").and_then(Value::as_i64) {
        if let Some(error) = value.get("error") {
            let failure = super::RpcFailure {
                code: error.get("code").and_then(Value::as_i64).unwrap_or(-1),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string(),
            };
            peer.complete(id, Err(failure));
        } else {
            peer.complete(id, Ok(value.get("result").cloned().unwrap_or(Value::Null)));
        }
        return;
    }
    emitter.emit(
        AdapterEvent::Unknown {
            method: "codex/unknownFrame".into(),
            sample: value,
        },
        EventMeta::key("codex/unknown-frame"),
    );
}

fn handle_server_request(
    emitter: &Emitter,
    peer: &Arc<RpcPeer>,
    id: &Value,
    method: &str,
    params: Value,
) {
    let options = |allow_session: bool| {
        let mut items = vec![
            RuntimePermissionOption {
                option_id: "accept".into(),
                label: "允许一次".into(),
                kind: "allow".into(),
            },
            RuntimePermissionOption {
                option_id: "decline".into(),
                label: "拒绝".into(),
                kind: "deny".into(),
            },
            RuntimePermissionOption {
                option_id: "cancel".into(),
                label: "拒绝并中断本轮".into(),
                kind: "cancel".into(),
            },
        ];
        if allow_session {
            items.insert(
                1,
                RuntimePermissionOption {
                    option_id: "acceptForSession".into(),
                    label: "本会话内同类操作均允许".into(),
                    kind: "allow-always".into(),
                },
            );
        }
        items
    };
    match method {
        "item/commandExecution/requestApproval" => {
            let request_id = peer.park_server_request(id);
            let detail = params
                .get("command")
                .and_then(Value::as_str)
                .map(|command| preview(&json!(command), 1200))
                .or_else(|| {
                    params
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(|reason| preview(&json!(reason), 600))
                });
            emitter.emit(
                AdapterEvent::PermissionRequested {
                    native_request_id: request_id,
                    title: "Codex 请求执行命令".into(),
                    detail,
                    kind: "command".into(),
                    options: options(true),
                    tool_call_id: params
                        .get("itemId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    turn_id: params
                        .get("turnId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                },
                EventMeta::key(format!("codex/permission:{}", id_key(id))),
            );
        }
        "item/fileChange/requestApproval" => {
            let request_id = peer.park_server_request(id);
            emitter.emit(
                AdapterEvent::PermissionRequested {
                    native_request_id: request_id,
                    title: "Codex 请求修改文件".into(),
                    detail: params
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(|reason| preview(&json!(reason), 600))
                        .or_else(|| {
                            params
                                .get("grantRoot")
                                .and_then(Value::as_str)
                                .map(|root| format!("申请写入根目录：{}", preview(&json!(root), 400)))
                        }),
                    kind: "file-write".into(),
                    options: options(true),
                    tool_call_id: params
                        .get("itemId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    turn_id: params
                        .get("turnId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                },
                EventMeta::key(format!("codex/permission:{}", id_key(id))),
            );
        }
        // Legacy approval surfaces (still declared by 0.166.0). The option ids
        // are the legacy vocabulary, and the reply is still `{decision}`.
        "execCommandApproval" | "applyPatchApproval" => {
            let request_id = peer.park_server_request(id);
            let is_patch = method == "applyPatchApproval";
            emitter.emit(
                AdapterEvent::PermissionRequested {
                    native_request_id: request_id,
                    title: if is_patch {
                        "Codex 请求应用补丁".into()
                    } else {
                        "Codex 请求执行命令".into()
                    },
                    detail: params
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(|reason| preview(&json!(reason), 600)),
                    kind: if is_patch { "file-write" } else { "command" }.into(),
                    options: vec![
                        RuntimePermissionOption {
                            option_id: "approved".into(),
                            label: "允许一次".into(),
                            kind: "allow".into(),
                        },
                        RuntimePermissionOption {
                            option_id: "denied".into(),
                            label: "拒绝".into(),
                            kind: "deny".into(),
                        },
                        RuntimePermissionOption {
                            option_id: "abort".into(),
                            label: "拒绝并中断本轮".into(),
                            kind: "cancel".into(),
                        },
                    ],
                    tool_call_id: params
                        .get("callId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    turn_id: params
                        .get("turnId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                },
                EventMeta::key(format!("codex/permission:{}", id_key(id))),
            );
        }
        other => {
            // Not implemented here: answered as a protocol-level "unsupported"
            // instead of pretending to be a permission card or hanging.
            let _ = peer.respond_error(
                id,
                -32601,
                "this client does not implement that server request",
            );
            emitter.emit(
                AdapterEvent::Unknown {
                    method: other.to_string(),
                    sample: json!({ "handled": false, "answeredWith": "methodNotFound" }),
                },
                EventMeta::key(format!("codex/unsupported:{other}")),
            );
        }
    }
}

fn handle_notification(
    emitter: &Emitter,
    thread_id: &Arc<Mutex<Option<String>>>,
    method: &str,
    params: Value,
) {
    match method {
        "item/agentMessage/delta" => {
            let item_id = params
                .get("itemId")
                .and_then(Value::as_str)
                .unwrap_or("assistant")
                .to_string();
            let delta = params
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if delta.is_empty() {
                return;
            }
            emitter.emit(
                AdapterEvent::TextDelta {
                    message_id: item_id,
                    index: 0,
                    text: delta,
                },
                EventMeta::key(format!("codex/agent-delta:{}", uuid_like())),
            );
        }
        "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
            let item_id = params
                .get("itemId")
                .and_then(Value::as_str)
                .unwrap_or("reasoning")
                .to_string();
            let delta = params
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if delta.is_empty() {
                return;
            }
            emitter.emit(
                AdapterEvent::ReasoningDelta {
                    message_id: item_id,
                    index: 0,
                    text: delta,
                },
                EventMeta::key(format!("codex/reasoning:{}", uuid_like())),
            );
        }
        "item/started" | "item/completed" => {
            let completed = method == "item/completed";
            let item = params.get("item").cloned().unwrap_or_else(|| json!({}));
            if let Some(event) = item_event(&item, completed) {
                let item_id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("item")
                    .to_string();
                let suffix = if completed { "completed" } else { "started" };
                emitter.emit(event, EventMeta::key(format!("codex/item:{item_id}:{suffix}")));
            }
        }
        "turn/started" => {
            let Some(turn) = params.get("turn") else {
                return;
            };
            let turn_id = turn
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if turn_id.is_empty() {
                return;
            }
            if let Some(thread) = params.get("threadId").and_then(Value::as_str) {
                *thread_id.lock() = Some(thread.to_string());
            }
            emitter.emit(
                AdapterEvent::TurnStarted {
                    turn_id: turn_id.clone(),
                    model: None,
                    effort: None,
                },
                EventMeta::key(format!("codex/turn-started:{turn_id}")),
            );
        }
        "turn/completed" => {
            let Some(turn) = params.get("turn") else {
                return;
            };
            let turn_id = turn
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let raw = turn
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let status = match raw {
                "completed" => "completed",
                "interrupted" => "cancelled",
                "failed" => "failed",
                other => other,
            };
            let reason = turn
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_string);
            emitter.emit(
                AdapterEvent::TurnCompleted {
                    turn_id: turn_id.clone(),
                    status: status.to_string(),
                    reason,
                },
                EventMeta::key(format!("codex/turn-completed:{turn_id}")),
            );
        }
        "error" => {
            let message = params
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("the Codex server reported an error");
            let will_retry = params
                .get("willRetry")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            emitter.emit(
                AdapterEvent::Warning {
                    code: if will_retry {
                        "serverErrorRetrying".into()
                    } else {
                        "serverError".into()
                    },
                    message: message.to_string(),
                },
                EventMeta::key(format!("codex/error:{}", uuid_like())),
            );
        }
        other if NOISE.contains(&other) => {
            // One sample per method is kept so a report can explain what the
            // server said; the repeated stream is not stored.
            emitter.emit(
                AdapterEvent::Unknown {
                    method: other.to_string(),
                    sample: json!({ "ignored": true }),
                },
                EventMeta::key(format!("codex/noise:{other}")),
            );
        }
        other => {
            emitter.emit(
                AdapterEvent::Unknown {
                    method: other.to_string(),
                    sample: params,
                },
                EventMeta::key(format!("codex/unknown:{other}")),
            );
        }
    }
}

/// Maps one `ThreadItem` onto a tool call or a completed message.
fn item_event(item: &Value, completed: bool) -> Option<AdapterEvent> {
    let id = item.get("id").and_then(Value::as_str)?.to_string();
    match item.get("type").and_then(Value::as_str)? {
        "agentMessage" => {
            if !completed {
                return None;
            }
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if text.is_empty() {
                return None;
            }
            Some(AdapterEvent::TextCompleted {
                message_id: id,
                role: "assistant".into(),
                text,
            })
        }
        "commandExecution" => {
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("inProgress");
            let exit_code = item.get("exitCode").and_then(Value::as_i64);
            if !completed {
                return Some(AdapterEvent::ToolCall {
                    tool_call_id: id,
                    name: "shell".into(),
                    kind: "command".into(),
                    input: Some(json!({
                        "command": item.get("command").cloned().unwrap_or(Value::Null),
                        "cwd": item.get("cwd").cloned().unwrap_or(Value::Null),
                    })),
                    permission_id: None,
                });
            }
            Some(AdapterEvent::ToolUpdate {
                tool_call_id: id,
                status: tool_status(status),
                output: item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .map(|text| preview(&json!(text), 4000)),
                is_error: exit_code.is_some_and(|code| code != 0) || status == "failed",
            })
        }
        "fileChange" => {
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("inProgress");
            if !completed {
                return Some(AdapterEvent::ToolCall {
                    tool_call_id: id,
                    name: "apply_patch".into(),
                    kind: "file-change".into(),
                    input: item.get("changes").cloned(),
                    permission_id: None,
                });
            }
            let summary = item
                .get("changes")
                .map(|changes| {
                    preview(
                        &json!(changes
                            .as_array()
                            .map(|items| items
                                .iter()
                                .filter_map(|change| change
                                    .get("path")
                                    .and_then(Value::as_str)
                                    .map(str::to_string))
                                .collect::<Vec<_>>())
                            .unwrap_or_default()),
                        2000,
                    )
                })
                .unwrap_or_default();
            Some(AdapterEvent::ToolUpdate {
                tool_call_id: id,
                status: tool_status(status),
                output: Some(summary),
                is_error: status == "failed",
            })
        }
        "mcpToolCall" => {
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("inProgress");
            let name = format!(
                "{}·{}",
                item.get("server").and_then(Value::as_str).unwrap_or("mcp"),
                item.get("tool").and_then(Value::as_str).unwrap_or("tool")
            );
            if !completed {
                return Some(AdapterEvent::ToolCall {
                    tool_call_id: id,
                    name,
                    kind: "mcp".into(),
                    input: item.get("arguments").cloned(),
                    permission_id: None,
                });
            }
            Some(AdapterEvent::ToolUpdate {
                tool_call_id: id,
                status: tool_status(status),
                output: item
                    .get("result")
                    .map(|result| preview(result, 4000))
                    .or_else(|| {
                        item.pointer("/error/message")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    }),
                is_error: item.get("error").is_some_and(|value| !value.is_null()),
            })
        }
        "webSearch" => {
            if !completed {
                return Some(AdapterEvent::ToolCall {
                    tool_call_id: id,
                    name: "web_search".into(),
                    kind: "web".into(),
                    input: Some(json!({
                        "query": item.get("query").cloned().unwrap_or(Value::Null),
                    })),
                    permission_id: None,
                });
            }
            Some(AdapterEvent::ToolUpdate {
                tool_call_id: id,
                status: "completed".into(),
                output: item.get("results").map(|value| preview(value, 2000)),
                is_error: false,
            })
        }
        // Reasoning and plan items are already delivered as deltas; a
        // user message is the one this client sent.
        _ => None,
    }
}

/// A unique-enough suffix for a delta that carries no id of its own. Deltas are
/// ordered and never retried by the server, so a local counter is sufficient.
fn uuid_like() -> String {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect() -> (Emitter, Arc<Mutex<Vec<(AdapterEvent, EventMeta)>>>) {
        let bucket = Arc::new(Mutex::new(Vec::new()));
        let sink_bucket = Arc::clone(&bucket);
        let emitter = Emitter::new(Arc::new(move |event, meta| {
            sink_bucket.lock().push((event, meta));
        }));
        (emitter, bucket)
    }

    #[test]
    fn command_execution_item_maps_to_a_tool_call_then_a_result() {
        let started = json!({
            "id": "item-1",
            "type": "commandExecution",
            "command": "echo hi",
            "cwd": "C:\\tmp",
            "status": "inProgress",
        });
        match item_event(&started, false).expect("tool call") {
            AdapterEvent::ToolCall {
                tool_call_id,
                kind,
                ..
            } => {
                assert_eq!(tool_call_id, "item-1");
                assert_eq!(kind, "command");
            }
            other => panic!("unexpected {other:?}"),
        }
        let done = json!({
            "id": "item-1",
            "type": "commandExecution",
            "status": "failed",
            "exitCode": 1,
            "aggregatedOutput": "boom",
        });
        match item_event(&done, true).expect("tool update") {
            AdapterEvent::ToolUpdate {
                status,
                is_error,
                output,
                ..
            } => {
                assert_eq!(status, "failed");
                assert!(is_error);
                assert_eq!(output.as_deref(), Some("boom"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn agent_message_item_completes_text_only_when_finished() {
        let item = json!({"id": "m-1", "type": "agentMessage", "text": "你好"});
        assert!(item_event(&item, false).is_none());
        match item_event(&item, true).expect("completed") {
            AdapterEvent::TextCompleted {
                message_id, text, ..
            } => {
                assert_eq!(message_id, "m-1");
                assert_eq!(text, "你好");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn approval_request_becomes_a_card_with_real_option_ids() {
        let (emitter, bucket) = collect();
        let peer = Arc::new(RpcPeer::new(Arc::new(fake_process())));
        handle_server_request(
            &emitter,
            &peer,
            &json!(7),
            "item/fileChange/requestApproval",
            json!({"itemId": "item-9", "turnId": "turn-1", "reason": "写入 tmp"}),
        );
        let events = bucket.lock();
        assert_eq!(events.len(), 1);
        match &events[0].0 {
            AdapterEvent::PermissionRequested {
                native_request_id,
                options,
                kind,
                tool_call_id,
                ..
            } => {
                assert_eq!(kind, "file-write");
                assert_eq!(tool_call_id.as_deref(), Some("item-9"));
                let ids: Vec<&str> = options.iter().map(|o| o.option_id.as_str()).collect();
                assert_eq!(ids, ["accept", "acceptForSession", "decline", "cancel"]);
                assert_eq!(native_request_id, "n:7");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(peer.pending_server_requests(), 1);
        // The parked id is removed once answered, so a second answer fails.
        assert!(peer.take_server_request("n:7").is_some());
        assert!(peer.take_server_request("n:7").is_none());
    }

    #[test]
    fn unsupported_server_request_is_answered_instead_of_hanging() {
        let (emitter, bucket) = collect();
        let peer = Arc::new(RpcPeer::new(Arc::new(fake_process())));
        handle_server_request(
            &emitter,
            &peer,
            &json!(11),
            "item/tool/requestUserInput",
            json!({}),
        );
        assert_eq!(peer.pending_server_requests(), 0);
        let events = bucket.lock();
        match &events[0].0 {
            AdapterEvent::Unknown { method, .. } => {
                assert_eq!(method, "item/tool/requestUserInput")
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn turn_completed_maps_interrupted_to_cancelled() {
        let (emitter, bucket) = collect();
        let thread = Arc::new(Mutex::new(None));
        handle_notification(
            &emitter,
            &thread,
            "turn/completed",
            json!({"turn": {"id": "t-1", "status": "interrupted"}}),
        );
        let events = bucket.lock();
        match &events[0].0 {
            AdapterEvent::TurnCompleted { status, .. } => assert_eq!(status, "cancelled"),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// A process handle that cannot be spawned in a unit test. The approval
    /// paths under test never write to it, so the fake is only used for the
    /// request bookkeeping.
    fn fake_process() -> OwnedProcess {
        OwnedProcess::detached_for_tests()
    }
}
