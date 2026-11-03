//! CodeBuddy Code adapter — Agent Client Protocol over stdio (`--acp`).
//!
//! Verified on this machine against `2.179.0` (`scratch/S05/probe-codebuddy/`):
//!
//! - `initialize` returns `protocolVersion: 1` (a **number**, not the string the
//!   bundled doc shows) plus `agentCapabilities` with `loadSession: true`;
//! - `session/new` takes `cwd` (not the documented `workingDirectory`);
//! - `session/load` and `session/resume` both require `sessionId` **and** `cwd`
//!   **and** `mcpServers`; with only `sessionId` the server answers `-32602`;
//! - `session/prompt` reports a failure as `result.stopReason = "refusal"` plus
//!   a JSON string inside `result._meta["codebuddy.ai/errorMessage"]`, not as a
//!   JSON-RPC error.
//!
//! Two environment facts matter more than the protocol: the ACP stdio entry
//! point still binds a local HTTP port, so an inherited `SERVER__PORT` that the
//! host already occupies makes it exit instantly with no stdout at all; and the
//! host may inject a headless bundle selector. Both are handled as a
//! **process-local** environment delta.
//!
//! Permission requests are handled on both surfaces the product declares: the
//! ACP `session/request_permission` request, and CodeBuddy's own
//! `interruption_request` update answered through
//! `_codebuddy.ai/resolveInterruption`. Neither could be observed live on this
//! machine because the standalone CLI has no login state, so this adapter
//! reports `unverified` rather than claiming a flow it never saw.

use std::collections::HashSet;
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

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(90);

/// Environment variables the ACP child must not inherit. `SERVER__PORT` is the
/// one that turns a working install into "no stdout, exit 1".
pub const ENV_REMOVE: &[&str] = &[
    "SERVER__PORT",
    "SERVER__HOST",
    "CODEBUDDY_FORCE_HEADLESS_BUNDLE",
    "CODEBUDDY_FORCE_LITE_WB_BUNDLE",
    "CODEBUDDY_MCP_CONFIG",
];

/// Explicitly disabled headless/lite selectors, process-local.
pub const ENV_SET: &[(&str, &str)] = &[("FORCE_HEADLESS", "0"), ("LITE", "0")];

pub struct CodeBuddyAdapter {
    peer: Arc<RpcPeer>,
    process: Arc<OwnedProcess>,
    emitter: Emitter,
    session_id: Mutex<Option<String>>,
    /// `session/prompt` request ids, so the reply can be reported as the end of
    /// a turn without blocking the caller.
    prompt_ids: PromptIds,
    text: Arc<Mutex<AcpText>>,
    pending_permissions: PendingPermissions,
    /// Login methods the engine advertised in `initialize`. Kept only to make an
    /// auth failure legible; this client never calls `authenticate`, because
    /// the installed one forces a logout of the working interactive login.
    auth_methods: Arc<Mutex<Vec<String>>>,
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
    closing: AtomicBool,
}

#[derive(Clone)]
struct PendingPermission {
    /// The id the UI saw.
    permission_id: String,
    /// `acp` when the server used `session/request_permission`, `interruption`
    /// when it used CodeBuddy's own update.
    surface: &'static str,
    /// JSON-RPC id for the ACP surface, `toolCallId` for the interruption one.
    native_id: String,
    options: Vec<RuntimePermissionOption>,
    created_at: std::time::Instant,
}

impl PendingPermission {
    fn offers(&self, option_id: &str) -> bool {
        self.options
            .iter()
            .any(|option| option.option_id == option_id)
    }
}

/// Requests the peer is still waiting on, keyed by the id the UI shows.
pub type PendingPermissions = Arc<Mutex<Vec<PendingPermission>>>;

/// ACP chunks have no offsets. Assign character offsets in receive order and
/// give each prompt its own identity; index=0 would discard later chunks.
struct AcpText {
    message_id: String,
    body: String,
    thought_offset: usize,
    active: bool,
}
impl Default for AcpText {
    fn default() -> Self {
        Self {
            message_id: local_key("codebuddy/message"),
            body: String::new(),
            thought_offset: 0,
            active: false,
        }
    }
}
impl AcpText {
    fn for_prompt() -> Self {
        Self {
            active: true,
            ..Self::default()
        }
    }
}
fn finish_text(emitter: &Emitter, state: &Arc<Mutex<AcpText>>) {
    let completed = {
        let mut text = state.lock();
        text.active = false;
        (text.message_id.clone(), std::mem::take(&mut text.body))
    };
    if !completed.1.is_empty() {
        emitter.emit(
            AdapterEvent::TextCompleted {
                message_id: completed.0.clone(),
                role: "assistant".into(),
                text: completed.1,
            },
            EventMeta::key(format!("codebuddy/message-completed:{}", completed.0)),
        );
    }
}

/// A request that nobody answered within this window is dropped: keeping it
/// would grow the registry for the life of the process and let a very late
/// answer reach a CLI that has moved on.
const PENDING_TTL: Duration = Duration::from_secs(30 * 60);

fn prune_pending(list: &mut Vec<PendingPermission>) {
    let now = std::time::Instant::now();
    list.retain(|pending| now.duration_since(pending.created_at) < PENDING_TTL);
}

pub struct CodeBuddyOptions {
    pub cwd: String,
    /// `Some(session_id)` continues an existing ACP session.
    pub resume_session: Option<String>,
    /// `true` when the caller wants `session/load` instead of `session/resume`.
    pub load_only: bool,
}

impl CodeBuddyAdapter {
    pub fn connect(
        process: Arc<OwnedProcess>,
        stdout: std::process::ChildStdout,
        emitter: Emitter,
        options: CodeBuddyOptions,
    ) -> Result<Arc<Self>, AppError> {
        let peer = Arc::new(RpcPeer::new(Arc::clone(&process)));
        let adapter = Arc::new(Self {
            peer: Arc::clone(&peer),
            process,
            emitter,
            session_id: Mutex::new(None),
            prompt_ids: Arc::new(Mutex::new(HashSet::new())),
            text: Arc::new(Mutex::new(AcpText::default())),
            pending_permissions: Arc::new(Mutex::new(Vec::new())),
            auth_methods: Arc::new(Mutex::new(Vec::new())),
            reader: Mutex::new(None),
            closing: AtomicBool::new(false),
        });

        let reader_emitter = adapter.emitter.clone();
        let reader_peer = Arc::clone(&peer);
        let reader_prompts: PromptIds = Arc::clone(&adapter.prompt_ids);
        let reader_pending: PendingPermissions = Arc::clone(&adapter.pending_permissions);
        let reader_auth = Arc::clone(&adapter.auth_methods);
        let reader_text = Arc::clone(&adapter.text);
        let handle = std::thread::Builder::new()
            .name("multicli-codebuddy-reader".into())
            .spawn(move || {
                let mut reader = LineReader::new(stdout);
                loop {
                    match reader.next_line() {
                        Ok(None) => break,
                        Ok(Some(line)) => match parse_line(&line) {
                            Ok(value) => dispatch_with_prompts(
                                &reader_emitter,
                                &reader_peer,
                                &reader_prompts,
                                &reader_pending,
                                &reader_auth,
                                &reader_text,
                                value,
                            ),
                            Err(error) => reader_emitter.emit(
                                AdapterEvent::Warning {
                                    code: "unparsableLine".into(),
                                    message: error,
                                },
                                EventMeta::key(local_key("codebuddy/unparsable")),
                            ),
                        },
                        Err(error) => {
                            reader_emitter.emit(
                                AdapterEvent::ProtocolError {
                                    message: error.message,
                                },
                                EventMeta::key("codebuddy/read-error"),
                            );
                            break;
                        }
                    }
                }
                finish_text(&reader_emitter, &reader_text);
                reader_peer.fail_all("the CodeBuddy ACP stream ended");
                reader_emitter.emit(
                    AdapterEvent::Exited {
                        code: None,
                        reason: Some("CodeBuddy ACP 输出流已结束".into()),
                    },
                    EventMeta::key("codebuddy/eof"),
                );
            })
            .map_err(|error| {
                AppError::new(
                    ErrorCode::Io,
                    format!("failed to start the CodeBuddy reader thread: {error}"),
                )
                .with_details(json!({ "stage": "spawnReader" }))
            })?;
        *adapter.reader.lock() = Some(handle);

        adapter.handshake(&options)?;
        Ok(adapter)
    }

    fn handshake(&self, options: &CodeBuddyOptions) -> Result<(), AppError> {
        let init = self.peer.call(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    // This client does not serve file reads/writes for the
                    // agent: the CLI keeps its own tools, and every write goes
                    // through its own permission prompt.
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false,
                },
            }),
            HANDSHAKE_TIMEOUT,
        )?;
        {
            let declared: Vec<String> = init
                .pointer("/authMethods")
                .and_then(Value::as_array)
                .map(|methods| {
                    methods
                        .iter()
                        .filter_map(|method| method.get("id").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            *self.auth_methods.lock() = declared;
        }
        let load_session = init
            .pointer("/agentCapabilities/loadSession")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // Only a declared capability authorises a method. `loadSession` means
        // "load an existing session", not "resume extension present": 2.179.0
        // declares `loadSession` and no resume capability, so `session/resume`
        // is never sent. Probing it and falling back on -32601 is exactly the
        // "try methods until one works" behaviour this stage forbids.
        let resume_declared = init
            .pointer("/agentCapabilities/resumeSession")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let session_id = match &options.resume_session {
            Some(existing) => {
                if !load_session {
                    return Err(AppError::new(
                        ErrorCode::Unsupported,
                        "this CodeBuddy build does not declare loadSession; continuing is not offered",
                    )
                    .with_details(json!({
                        "stage": "initialize",
                        "loadSession": false,
                        "resumeDeclared": resume_declared,
                    })));
                }
                let method = if resume_declared && !options.load_only {
                    "session/resume"
                } else {
                    "session/load"
                };
                // `cwd` and `mcpServers` are both required: with only a
                // sessionId the server answers -32602.
                self.peer.call(
                    method,
                    json!({
                        "sessionId": existing,
                        "cwd": options.cwd,
                        "mcpServers": [],
                    }),
                    HANDSHAKE_TIMEOUT,
                )?;
                existing.clone()
            }
            None => {
                let reply = self.peer.call(
                    "session/new",
                    json!({ "cwd": options.cwd, "mcpServers": [] }),
                    HANDSHAKE_TIMEOUT,
                )?;
                reply
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| {
                        AppError::new(ErrorCode::Protocol, "session/new returned no sessionId")
                            .with_details(json!({ "stage": "sessionNew" }))
                    })?
            }
        };
        *self.session_id.lock() = Some(session_id.clone());
        self.emitter.emit(
            AdapterEvent::SessionBound {
                native_session_id: session_id,
            },
            EventMeta::key("codebuddy/session-bound"),
        );
        Ok(())
    }

    fn ensure_session(&self) -> Result<String, AppError> {
        self.session_id.lock().clone().ok_or_else(|| {
            AppError::conflict("run", "the ACP session is not open yet")
                .with_details(json!({ "stage": "acpSession" }))
        })
    }
}

impl Adapter for CodeBuddyAdapter {
    fn native_session_id(&self) -> Option<String> {
        self.session_id.lock().clone()
    }

    fn send_prompt(&self, text: &str) -> Result<(), AppError> {
        let session_id = self.ensure_session()?;
        *self.text.lock() = AcpText::for_prompt();
        // The reply arrives when the turn ends, so this must not wait: doing so
        // would block the IPC command for the whole turn.
        let mut registered = None;
        let sent = self.peer.send_async_registered(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": text }],
            }),
            |id| {
                registered = Some(id);
                self.prompt_ids.lock().insert(id);
                self.emitter.emit(
                    AdapterEvent::TurnStarted {
                        turn_id: format!("prompt-{id}"),
                        model: None,
                        effort: None,
                    },
                    EventMeta::key(format!("codebuddy/turn:{id}")),
                );
            },
        );
        match sent {
            Ok(_) => {}
            Err(error) => {
                if let Some(id) = registered {
                    self.prompt_ids.lock().remove(&id);
                }
                return Err(error);
            }
        }
        Ok(())
    }

    fn cancel_turn(&self, reason: &str) -> Result<(), AppError> {
        // Answer what we still hold before dropping the table. Clearing it
        // alone leaves the peer blocked on every parked ACP id, and the id is
        // gone from our side too — the request can never be answered, and a
        // later deny obligation fails with "no longer waiting".
        let pending = std::mem::take(&mut *self.pending_permissions.lock());
        let session_id = self.ensure_session()?;
        for request in pending {
            if request.surface != "acp" {
                // The interruption surface is cancelled through
                // `session/cancel`; the product owns that queue.
                continue;
            }
            let Some(raw) = self.peer.take_server_request(&request.native_id) else {
                continue;
            };
            let deny = request
                .options
                .iter()
                .find(|option| option.kind.starts_with("deny"))
                .or_else(|| {
                    request
                        .options
                        .iter()
                        .find(|option| option.kind == "cancel")
                })
                .map(|option| option.option_id.clone());
            if let Some(option_id) = deny {
                let _ = self.peer.respond(
                    &raw,
                    json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
                );
            }
        }
        self.peer
            .notify("session/cancel", json!({ "sessionId": session_id }))?;
        self.emitter.emit(
            AdapterEvent::Warning {
                code: "cancelRequested".into(),
                message: format!("已请求 CodeBuddy 取消当前回合：{reason}"),
            },
            EventMeta::key(local_key("codebuddy/cancel")),
        );
        Ok(())
    }

    fn answer_permission(
        &self,
        native_request_id: &str,
        option_id: &str,
        _kind: &str,
    ) -> Result<(), AppError> {
        let session_id = self.ensure_session()?;
        let mut list = self.pending_permissions.lock();
        prune_pending(&mut list);
        let position = list
            .iter()
            .position(|pending| pending.permission_id == native_request_id)
            .ok_or_else(|| {
                AppError::conflict(
                    "permission",
                    "this ACP request is no longer waiting for an answer",
                )
                .with_details(json!({ "requestId": native_request_id }))
            })?;
        // Validate **before** removing: an option the request never offered must
        // not consume it.
        if !list[position].offers(option_id) {
            let offered: Vec<String> = list[position]
                .options
                .iter()
                .map(|option| option.option_id.clone())
                .collect();
            return Err(AppError::invalid_argument(
                "optionId",
                "must be one of the options the ACP request offered",
            )
            .with_details(json!({
                "requestId": native_request_id,
                "offered": offered,
            })));
        }
        let pending = list.remove(position);
        drop(list);

        let result = match pending.surface {
            "acp" => {
                let raw = self
                    .peer
                    .take_server_request(&pending.native_id)
                    .ok_or_else(|| {
                        AppError::conflict(
                            "permission",
                            "the ACP request id is no longer parked on the wire",
                        )
                        .with_details(json!({ "requestId": native_request_id }))
                    })?;
                let written = self.peer.respond(
                    &raw,
                    json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
                );
                if written.is_err() {
                    // Put the wire id back under the same key: without it the
                    // retry path has no request to answer, even though the
                    // pending entry was restored.
                    self.peer.repark_server_request(&pending.native_id, raw);
                }
                written
            }
            _ => {
                // CodeBuddy's own decision vocabulary, declared by its bundled
                // ACP documentation.
                let decision = match option_id {
                    "allow" => "allow",
                    "allowAll" => "allowAll",
                    _ => "deny",
                };
                let reply = self.peer.call(
                    "_codebuddy.ai/resolveInterruption",
                    json!({
                        "sessionId": session_id,
                        "toolCallId": pending.native_id,
                        "decision": decision,
                        "answers": {},
                    }),
                    Duration::from_secs(30),
                );
                match reply {
                    Ok(reply) => {
                        // Three-state answer, not a boolean. The product returns
                        // `{resolved:false, queued:true}` from an early-exit
                        // branch when no interruption is active yet: recorded,
                        // not applied.
                        let state = interruption_decision_state(&reply);
                        if state != "applied" {
                            // Submitted, not applied. Both layers are told: the
                            // warning explains it, and the event keeps the run
                            // from closing the answer path while claiming the
                            // request is still pending.
                            self.emitter.emit(
                                AdapterEvent::Warning {
                                    code: format!("permissionNotApplied:{state}"),
                                    message: format!(
                                        "CodeBuddy 未应用该决定（{state}）；请求保持在待答复状态"
                                    ),
                                },
                                EventMeta::key(local_key("codebuddy/permission-not-applied")),
                            );
                            self.emitter.emit(
                                AdapterEvent::PermissionNotApplied {
                                    native_request_id: native_request_id.to_string(),
                                    state: state.to_string(),
                                    detail: Some(
                                        "resolveInterruption 未把决定应用到该请求".to_string(),
                                    ),
                                },
                                EventMeta::key(local_key("codebuddy/permission-not-applied-event")),
                            );
                            // Not applied: put it back so the user can retry or
                            // cancel instead of losing the request.
                            let mut list = self.pending_permissions.lock();
                            list.push(pending.clone());
                        }
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
        };

        if let Err(error) = &result {
            // The wire write failed: keep the request answerable rather than
            // dropping it into a state nobody can act on.
            self.pending_permissions.lock().push(pending);
            self.emitter.emit(
                AdapterEvent::Warning {
                    code: "permissionAnswerNotSent".into(),
                    message: format!(
                        "权限应答未能送达 CodeBuddy（{}）；请求仍可重试",
                        error.message
                    ),
                },
                EventMeta::key(local_key("codebuddy/permission-answer-failed")),
            );
        }
        result
    }

    fn shutdown(&self) {
        if self.closing.swap(true, Ordering::SeqCst) {
            return;
        }
        self.pending_permissions.lock().clear();
        self.process.close_stdin();
        if let Some(handle) = self.reader.lock().take() {
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

/// Dispatch needs the adapter's prompt bookkeeping, so the reader thread calls
/// this with a shared set of ids that are waiting for a `session/prompt` reply.
pub type PromptIds = Arc<Mutex<HashSet<i64>>>;

fn dispatch_with_prompts(
    emitter: &Emitter,
    peer: &Arc<RpcPeer>,
    prompts: &PromptIds,
    pending: &PendingPermissions,
    auth: &Arc<Mutex<Vec<String>>>,
    text: &Arc<Mutex<AcpText>>,
    value: Value,
) {
    if let Some(id) = value.get("id").and_then(Value::as_i64) {
        let is_prompt = {
            let mut ids = prompts.lock();
            ids.remove(&id)
        };
        if is_prompt {
            finish_text(emitter, text);
            if let Some(error) = value.get("error") {
                emitter.emit(
                    AdapterEvent::TurnCompleted {
                        turn_id: format!("prompt-{id}"),
                        status: "failed".into(),
                        reason: Some(
                            error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("session/prompt failed")
                                .to_string(),
                        ),
                    },
                    EventMeta::key(format!("codebuddy/prompt-error:{id}")),
                );
                return;
            }
            let result = value.get("result").cloned().unwrap_or(Value::Null);
            let declared = auth.lock().clone();
            let (status, reason, _cancelled) = prompt_outcome_with_auth(&result, &declared);
            emitter.emit(
                AdapterEvent::TurnCompleted {
                    turn_id: format!("prompt-{id}"),
                    status,
                    reason,
                },
                EventMeta::key(format!("codebuddy/prompt-done:{id}")),
            );
            return;
        }
    }
    dispatch(emitter, peer, pending, text, value);
}

fn dispatch(
    emitter: &Emitter,
    peer: &Arc<RpcPeer>,
    pending: &PendingPermissions,
    text: &Arc<Mutex<AcpText>>,
    value: Value,
) {
    if let Some(method) = value.get("method").and_then(Value::as_str) {
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        match value.get("id") {
            Some(id) => handle_server_request(emitter, peer, pending, id, method, params),
            None => handle_notification(emitter, pending, text, method, params),
        }
        return;
    }
    if let Some(id) = value.get("id").and_then(Value::as_i64) {
        if let Some(error) = value.get("error") {
            peer.complete(
                id,
                Err(super::RpcFailure {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(-1),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_string(),
                }),
            );
        } else {
            peer.complete(id, Ok(value.get("result").cloned().unwrap_or(Value::Null)));
        }
        return;
    }
    emitter.emit(
        AdapterEvent::Unknown {
            method: "codebuddy/unknownFrame".into(),
            sample: value,
        },
        EventMeta::key("codebuddy/unknown-frame"),
    );
}

fn handle_server_request(
    emitter: &Emitter,
    peer: &Arc<RpcPeer>,
    pending: &PendingPermissions,
    id: &Value,
    method: &str,
    params: Value,
) {
    match method {
        "session/request_permission" => {
            let parked = peer.park_server_request(id);
            let permission_id = format!("acp-perm-{}", local_key("p"));
            let options: Vec<RuntimePermissionOption> = params
                .get("options")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            let option_id = item.get("optionId").and_then(Value::as_str)?;
                            let raw_kind = item
                                .get("kind")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown");
                            Some(RuntimePermissionOption {
                                option_id: option_id.to_string(),
                                label: item
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                                    .unwrap_or_else(|| option_id.to_string()),
                                kind: match raw_kind {
                                    "allow_once" => "allow",
                                    "allow_always" => "allow-always",
                                    "reject_once" => "deny",
                                    "reject_always" => "deny-always",
                                    other => other,
                                }
                                .to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if options.is_empty() {
                // A permission request with no options cannot be answered
                // honestly; it is refused instead of invented.
                let _ = peer.respond_error(id, -32602, "no options were offered");
                emitter.emit(
                    AdapterEvent::Unknown {
                        method: "session/request_permission".into(),
                        sample: json!({ "handled": false, "reason": "no options" }),
                    },
                    EventMeta::key(local_key("codebuddy/perm-no-options")),
                );
                return;
            }
            {
                let mut list = pending.lock();
                prune_pending(&mut list);
                list.push(PendingPermission {
                    permission_id: permission_id.clone(),
                    surface: "acp",
                    // The parked key, not the raw id: `answer` looks it up here.
                    native_id: parked.clone(),
                    options: options.clone(),
                    created_at: std::time::Instant::now(),
                });
            }
            emitter.emit(
                AdapterEvent::PermissionRequested {
                    native_request_id: permission_id,
                    title: "CodeBuddy 请求权限".into(),
                    detail: params
                        .pointer("/toolCall/title")
                        .and_then(Value::as_str)
                        .map(|title| preview(&json!(title), 600)),
                    kind: "tool".into(),
                    options,
                    tool_call_id: params
                        .pointer("/toolCall/toolCallId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    turn_id: None,
                },
                EventMeta::key(format!("codebuddy/permission:{}", id_key(id))),
            );
        }
        other => {
            let _ = peer.respond_error(
                id,
                -32601,
                "this client does not implement that ACP server request",
            );
            emitter.emit(
                AdapterEvent::Unknown {
                    method: other.to_string(),
                    sample: json!({ "answeredWith": "methodNotFound" }),
                },
                EventMeta::key(format!("codebuddy/unsupported:{other}")),
            );
        }
    }
}

fn handle_notification(
    emitter: &Emitter,
    pending: &PendingPermissions,
    text: &Arc<Mutex<AcpText>>,
    method: &str,
    params: Value,
) {
    match method {
        "session/update" => {
            let update = params.get("update").cloned().unwrap_or_else(|| json!({}));
            handle_update(emitter, pending, text, &update);
        }
        "_codebuddy.ai/command" => emitter.emit(
            AdapterEvent::Unknown {
                method: method.to_string(),
                sample: json!({ "ignored": true }),
            },
            EventMeta::key("codebuddy/command"),
        ),
        "_codebuddy.ai/modResidentInvalidate" => emitter.emit(
            AdapterEvent::Unknown {
                method: method.to_string(),
                sample: json!({ "ignored": true }),
            },
            EventMeta::key("codebuddy/mod-resident"),
        ),
        other => emitter.emit(
            AdapterEvent::Unknown {
                method: other.to_string(),
                sample: params,
            },
            EventMeta::key(format!("codebuddy/unknown:{other}")),
        ),
    }
}

/// Session metadata updates that carry no transcript content.
const UPDATE_NOISE: &[&str] = &[
    "config_option_update",
    "available_commands_update",
    "usage_update",
    "session_info_update",
    "current_mode_update",
    "mode_update",
    "model_update",
    "plan",
];

fn handle_update(
    emitter: &Emitter,
    pending: &PendingPermissions,
    stream: &Arc<Mutex<AcpText>>,
    update: &Value,
) {
    let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
        return;
    };
    // session/load replays historical text and tools before any prompt. Those
    // are not events of this new Run and must not become fresh transcript rows.
    if matches!(
        kind,
        "agent_message_chunk"
            | "agent_thought_chunk"
            | "user_message_chunk"
            | "tool_call"
            | "tool_call_update"
            | "interruption_request"
            | "session_end"
    ) && !stream.lock().active
    {
        return;
    }
    match kind {
        "agent_message_chunk" => {
            let text = chunk_text(update);
            if text.is_empty() {
                return;
            }
            let (message_id, index) = {
                let mut state = stream.lock();
                let index = state.body.chars().count();
                state.body.push_str(&text);
                (state.message_id.clone(), index)
            };
            emitter.emit(
                AdapterEvent::TextDelta {
                    message_id,
                    index,
                    text,
                },
                EventMeta::key(local_key("codebuddy/text")),
            );
        }
        "agent_thought_chunk" => {
            let text = chunk_text(update);
            if text.is_empty() {
                return;
            }
            let (message_id, index) = {
                let mut state = stream.lock();
                let index = state.thought_offset;
                state.thought_offset += text.chars().count();
                (format!("{}:reasoning", state.message_id), index)
            };
            emitter.emit(
                AdapterEvent::ReasoningDelta {
                    message_id,
                    index,
                    text,
                },
                EventMeta::key(local_key("codebuddy/thought")),
            );
        }
        "tool_call" => {
            let Some(tool_call_id) = update
                .get("toolCallId")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                return;
            };
            emitter.emit(
                AdapterEvent::ToolCall {
                    tool_call_id,
                    name: update
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                    kind: update
                        .get("kind")
                        .and_then(Value::as_str)
                        .map(|kind| match kind {
                            "edit" => "file-change".to_string(),
                            "execute" => "command".to_string(),
                            "search" | "read" => "search".to_string(),
                            other => other.to_string(),
                        })
                        .unwrap_or_else(|| "tool".into()),
                    input: update.get("rawInput").cloned(),
                    permission_id: None,
                },
                EventMeta::key(format!(
                    "codebuddy/tool:{}",
                    update
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                )),
            );
        }
        "tool_call_update" => {
            let Some(tool_call_id) = update
                .get("toolCallId")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                return;
            };
            let status = update
                .get("status")
                .and_then(Value::as_str)
                .map(tool_status)
                .unwrap_or_else(|| "running".into());
            emitter.emit(
                AdapterEvent::ToolUpdate {
                    tool_call_id: tool_call_id.clone(),
                    status: status.clone(),
                    output: update.get("rawOutput").map(|value| preview(value, 4000)),
                    is_error: status == "failed",
                },
                EventMeta::key(format!("codebuddy/tool-update:{tool_call_id}:{status}")),
            );
        }
        "interruption_request" => {
            let Some(tool_call_id) = update
                .get("toolCallId")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                return;
            };
            let permission_id = format!("int-{}", local_key("p"));
            let options = vec![
                RuntimePermissionOption {
                    option_id: "allow".into(),
                    label: "允许一次".into(),
                    kind: "allow".into(),
                },
                RuntimePermissionOption {
                    option_id: "allowAll".into(),
                    label: "本会话均允许".into(),
                    kind: "allow-always".into(),
                },
                RuntimePermissionOption {
                    option_id: "deny".into(),
                    label: "拒绝".into(),
                    kind: "deny".into(),
                },
            ];
            {
                let mut list = pending.lock();
                prune_pending(&mut list);
                list.push(PendingPermission {
                    permission_id: permission_id.clone(),
                    surface: "interruption",
                    native_id: tool_call_id.clone(),
                    options: options.clone(),
                    created_at: std::time::Instant::now(),
                });
            }
            emitter.emit(
                AdapterEvent::PermissionRequested {
                    native_request_id: permission_id,
                    title: "CodeBuddy 请求工具权限".into(),
                    detail: update
                        .get("title")
                        .and_then(Value::as_str)
                        .map(|title| preview(&json!(title), 600)),
                    kind: "tool".into(),
                    options,
                    tool_call_id: Some(tool_call_id.clone()),
                    turn_id: None,
                },
                EventMeta::key(format!("codebuddy/interruption:{tool_call_id}")),
            );
        }
        "user_message_chunk" => {}
        "session_end" => {
            emitter.emit(
                AdapterEvent::TurnCompleted {
                    turn_id: "session-end".into(),
                    status: "completed".into(),
                    reason: None,
                },
                EventMeta::key(local_key("codebuddy/session-end")),
            );
        }
        other if UPDATE_NOISE.contains(&other) => {
            emitter.emit(
                AdapterEvent::Unknown {
                    method: format!("session/update:{other}"),
                    sample: json!({ "ignored": true }),
                },
                EventMeta::key(format!("codebuddy/noise:{other}")),
            );
        }
        other => {
            emitter.emit(
                AdapterEvent::Unknown {
                    method: format!("session/update:{other}"),
                    sample: update.clone(),
                },
                EventMeta::key(format!("codebuddy/unknown-update:{other}")),
            );
        }
    }
}

/// The ACP `resolveInterruption` three-state answer, exposed for tests.
pub fn interruption_decision_state(reply: &Value) -> &'static str {
    let resolved = reply
        .get("resolved")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let queued = reply
        .get("queued")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match (resolved, queued) {
        (true, _) => "applied",
        (false, true) => "pending",
        (false, false) => "rejected",
    }
}

fn chunk_text(update: &Value) -> String {
    update
        .pointer("/content/text")
        .and_then(Value::as_str)
        .or_else(|| update.get("text").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// Reads the ACP stop reason out of a `session/prompt` reply and maps the
/// CodeBuddy-specific failure envelope onto a real status.
///
/// `stopReason` is not the three-value enum the bundled documentation shows:
/// the product's own code constructs seven (`end_turn`, `completed`,
/// `cancelled`, `interrupted`, `refusal`, `error`, `max_tokens`). Only the
/// observed ones are mapped by name; anything else is `failed` with the literal
/// value quoted, never silently treated as success.
///
/// `refusal` is **not** "the user denied a tool": it is produced on an error
/// path and by sensitive-input interception, so it maps to `failed`, never to
/// `denied`. A tool the user denied is recorded on the permission ledger, not
/// here.
pub fn prompt_outcome(result: &Value) -> (String, Option<String>, bool) {
    prompt_outcome_with_auth(result, &[])
}

/// Same mapping, with the engine's declared login methods appended to an auth
/// failure so the reason says *what the engine wants* instead of only what it
/// refused. The claim about credentials is deliberately narrow: the client did
/// not call `authenticate`, and whether the interactive login is reusable over
/// ACP has never been measured.
pub fn prompt_outcome_with_auth(
    result: &Value,
    auth_methods: &[String],
) -> (String, Option<String>, bool) {
    let stop = result
        .get("stopReason")
        .and_then(Value::as_str)
        .unwrap_or("end_turn");
    let auth_error = result
        .pointer("/_meta/codebuddy.ai~1errorMessage")
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    match stop {
        "end_turn" | "completed" => ("completed".into(), None, false),
        "cancelled" | "interrupted" => ("cancelled".into(), None, true),
        "refusal" => {
            let message = auth_error
                .as_ref()
                .and_then(|value| value.get("message").and_then(Value::as_str))
                .map(str::to_string)
                .unwrap_or_else(|| "the agent refused to continue".into());
            let auth_failure = auth_error
                .as_ref()
                .map(|value| {
                    value.pointer("/data/category").and_then(Value::as_str) == Some("auth")
                        || value.get("category").and_then(Value::as_str) == Some("auth")
                        || message.contains("Authentication required")
                })
                .unwrap_or(false);
            let reason = if auth_failure && !auth_methods.is_empty() {
                // Evidence only: the engine declared these methods and the
                // prompt was refused for auth. Why the interactive login is not
                // visible here is not established, so it is not asserted.
                format!(
                    "{message}（该引擎声明登录方式：{}；客户端未调用 authenticate，是否与交互式登录共用凭据未验证）",
                    auth_methods.join("/")
                )
            } else {
                message
            };
            ("failed".into(), Some(reason), false)
        }
        other => (
            "failed".into(),
            Some(format!("unexpected stopReason: {other}")),
            false,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_outcome_reads_the_nested_error_envelope() {
        let refusal = json!({
            "stopReason": "refusal",
            "userMessageId": "u-1",
            "_meta": {
                "codebuddy.ai/errorMessage":
                    "{\"code\":-32000,\"message\":\"Authentication required\",\"data\":{\"category\":\"auth\"}}"
            }
        });
        let (status, reason, cancelled) = prompt_outcome(&refusal);
        assert_eq!(status, "failed");
        assert_eq!(reason.as_deref(), Some("Authentication required"));
        assert!(!cancelled);

        let ok = json!({ "stopReason": "end_turn" });
        assert_eq!(prompt_outcome(&ok).0, "completed");

        let cancel = json!({ "stopReason": "cancelled" });
        assert_eq!(prompt_outcome(&cancel).1, None);
        assert!(prompt_outcome(&cancel).2);

        let odd = json!({ "stopReason": "something" });
        assert_eq!(prompt_outcome(&odd).0, "failed");
        assert_eq!(
            prompt_outcome(&odd).1.as_deref(),
            Some("unexpected stopReason: something"),
            "an unknown reason must be quoted, not guessed"
        );

        // The seven values the product actually constructs.
        assert_eq!(
            prompt_outcome(&json!({"stopReason": "completed"})).0,
            "completed"
        );
        assert_eq!(
            prompt_outcome(&json!({"stopReason": "interrupted"})).0,
            "cancelled"
        );
        assert_eq!(prompt_outcome(&json!({"stopReason": "error"})).0, "failed");
        assert_eq!(
            prompt_outcome(&json!({"stopReason": "max_tokens"})).0,
            "failed"
        );
        // A refusal is an error path, never a user denial.
        assert_eq!(
            prompt_outcome(&json!({"stopReason": "refusal"})).0,
            "failed"
        );
    }

    #[test]
    fn an_auth_refusal_names_the_declared_login_methods_and_admits_the_unknown() {
        let refusal = json!({
            "stopReason": "refusal",
            "_meta": {
                "codebuddy.ai/errorMessage":
                    "{\"code\":-32000,\"message\":\"Authentication required\",\"data\":{\"category\":\"auth\"}}"
            }
        });
        let methods = vec!["iOA".to_string(), "internal".to_string()];
        let (status, reason, cancelled) = prompt_outcome_with_auth(&refusal, &methods);
        assert_eq!(status, "failed");
        assert!(!cancelled);
        let reason = reason.unwrap_or_default();
        assert!(
            reason.contains("iOA/internal"),
            "the declared methods must be quoted: {reason}"
        );
        assert!(
            reason.contains("未验证"),
            "the reason must not assert why the interactive login is not shared: {reason}"
        );
        // Without a declaration nothing is invented.
        let plain = prompt_outcome(&refusal).1.unwrap_or_default();
        assert_eq!(plain, "Authentication required");
    }

    #[test]
    fn env_policy_covers_the_port_that_breaks_the_acp_child() {
        assert!(ENV_REMOVE.contains(&"SERVER__PORT"));
        assert!(ENV_REMOVE.contains(&"SERVER__HOST"));
        assert!(ENV_REMOVE.contains(&"CODEBUDDY_FORCE_HEADLESS_BUNDLE"));
        assert!(ENV_SET
            .iter()
            .any(|(name, value)| *name == "FORCE_HEADLESS" && *value == "0"));
    }

    #[test]
    fn interruption_ack_is_a_three_state_answer_not_a_boolean() {
        assert_eq!(
            interruption_decision_state(&json!({"resolved": true, "queued": false})),
            "applied"
        );
        // The early-exit branch the product really returns: recorded, not
        // applied. Reading this as "applied" would advance the ledger too soon.
        assert_eq!(
            interruption_decision_state(&json!({"resolved": false, "queued": true})),
            "pending"
        );
        assert_eq!(
            interruption_decision_state(&json!({"resolved": false, "queued": false})),
            "rejected"
        );
        // A reply with neither field is not evidence of anything.
        assert_eq!(interruption_decision_state(&json!({})), "rejected");
    }

    #[test]
    fn chunk_text_accepts_both_declared_shapes() {
        assert_eq!(
            chunk_text(&json!({"content": {"type": "text", "text": "hi"}})),
            "hi"
        );
        assert_eq!(chunk_text(&json!({"text": "yo"})), "yo");
        assert_eq!(chunk_text(&json!({})), "");
    }

    #[test]
    fn chunks_preserve_unicode_repetitions_and_separate_turns() {
        let deltas = Arc::new(Mutex::new(Vec::new()));
        let completed = Arc::new(Mutex::new(Vec::new()));
        let captured_deltas = Arc::clone(&deltas);
        let captured_completed = Arc::clone(&completed);
        let emitter = Emitter::new(Arc::new(move |event, _| match event {
            AdapterEvent::TextDelta {
                message_id,
                index,
                text,
            } => captured_deltas.lock().push((message_id, index, text)),
            AdapterEvent::TextCompleted {
                message_id, text, ..
            } => captured_completed.lock().push((message_id, text)),
            _ => {}
        }));
        let pending = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(Mutex::new(AcpText::default()));
        handle_update(
            &emitter,
            &pending,
            &state,
            &json!({"sessionUpdate":"agent_message_chunk","text":"old history replay"}),
        );
        assert!(deltas.lock().is_empty());
        *state.lock() = AcpText::for_prompt();
        for chunk in ["中😀", "文", "文"] {
            handle_update(
                &emitter,
                &pending,
                &state,
                &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":chunk}}),
            );
        }
        finish_text(&emitter, &state);
        finish_text(&emitter, &state);
        *state.lock() = AcpText::for_prompt();
        handle_update(
            &emitter,
            &pending,
            &state,
            &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"收到🎲"}}),
        );
        finish_text(&emitter, &state);
        let chunks = deltas.lock();
        let final_text = completed.lock();
        assert_eq!(
            chunks
                .iter()
                .map(|(_, index, _)| *index)
                .collect::<Vec<_>>(),
            vec![0, 2, 3, 0]
        );
        assert_eq!(
            final_text
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>(),
            vec!["中😀文文", "收到🎲"]
        );
        assert_ne!(final_text[0].0, final_text[1].0);
    }
}
