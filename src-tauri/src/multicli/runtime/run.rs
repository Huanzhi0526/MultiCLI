//! One managed structured run: adapter connection, event state machine,
//! permission registry and durable persistence.
//!
//! A Run is created by the user (a new conversation) or by an explicit
//! "continue" on a historical Session. It always owns exactly one CLI process
//! and one `streamId`. Consequences that the rest of the code depends on:
//!
//! - **A new turn is not a new Run.** Sending a follow-up message reuses the
//!   connection and keeps `(runId, streamId)`; only continuing a historical
//!   Session creates a new Run.
//! - **Late events die with their connection.** The sink captures the run's
//!   generation and the run drops anything tagged with an older one, so a
//!   killed process cannot answer a request that belongs to a newer run.
//! - **Every answer is checked against the same request.** A permission answer
//!   must name a `permissionId` that is still `pending` in *this* Run; a
//!   cancelled request can never be approved afterwards.
//! - **Nothing is inferred from silence.** `working` requires a protocol
//!   message; no output at all leaves the interaction state `unknown`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::multicli::domain::{
    EvidenceLevel, ExecutionStatus, InteractionStatus, Message, ProcessIdentity, Run, RunOwnership,
    RunTransport,
};
use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::MultiCliService;

use super::envelope::{kind, now_iso, EventJournal};
use super::model::{
    RuntimeMessageView, RuntimeNotify, RuntimePermissionOption, RuntimePermissionView,
    RuntimeRunView, RuntimeToolView, RuntimeTurnView, RUNTIME_SCHEMA_VERSION,
};

pub type NotifyFn = Arc<dyn Fn(RuntimeNotify) + Send + Sync>;

/// The CLI's own prefix for "the client's permission answer could not be
/// parsed". It arrives as an ordinary `tool_result`, so it is only meaningful
/// together with the ledger: this client must have answered a request for that
/// same tool. Matching the text alone would let document content be treated as a
/// fatal protocol error.

// ---------------------------------------------------------------------------
// Adapter surface
// ---------------------------------------------------------------------------

/// Metadata that makes an adapter event idempotent. Either the protocol's own
/// id, or a key the adapter guarantees to be stable for the same event.
#[derive(Debug, Clone, Default)]
pub struct EventMeta {
    pub native_event_id: Option<String>,
    pub source_key: Option<String>,
}

impl EventMeta {
    pub fn native(id: impl Into<String>) -> Self {
        Self {
            native_event_id: Some(id.into()),
            source_key: None,
        }
    }

    pub fn key(value: impl Into<String>) -> Self {
        Self {
            native_event_id: None,
            source_key: Some(value.into()),
        }
    }
}

/// A protocol fact, normalised. Adapters translate; they never decide product
/// policy (no auto-allow, no silent deny).
#[derive(Debug, Clone)]
pub enum AdapterEvent {
    SessionBound {
        native_session_id: String,
    },
    TurnStarted {
        turn_id: String,
        model: Option<String>,
        effort: Option<String>,
    },
    TextDelta {
        message_id: String,
        /// Character offset inside the message text.
        index: usize,
        text: String,
    },
    TextCompleted {
        message_id: String,
        role: String,
        text: String,
    },
    ReasoningDelta {
        message_id: String,
        index: usize,
        text: String,
    },
    ToolCall {
        tool_call_id: String,
        name: String,
        kind: String,
        input: Option<Value>,
        permission_id: Option<String>,
    },
    ToolUpdate {
        tool_call_id: String,
        status: String,
        output: Option<String>,
        is_error: bool,
    },
    PermissionRequested {
        native_request_id: String,
        title: String,
        detail: Option<String>,
        kind: String,
        options: Vec<RuntimePermissionOption>,
        tool_call_id: Option<String>,
        turn_id: Option<String>,
    },
    /// The CLI answered or withdrew the request on its own (for example the
    /// user pressed Ctrl+C in the terminal, or the turn ended).
    PermissionResolved {
        native_request_id: String,
        option_id: Option<String>,
        state: String,
    },
    /// The decision reached the CLI but was **not** applied. Distinct from
    /// `PermissionResolved`: the request stays answerable so the user can retry
    /// or cancel, and the view says so.
    PermissionNotApplied {
        native_request_id: String,
        /// `pending` (pre-registered) or `rejected` (refused outright).
        state: String,
        detail: Option<String>,
    },
    TurnCompleted {
        turn_id: String,
        /// `completed` | `cancelled` | `failed`
        status: String,
        reason: Option<String>,
    },
    Warning {
        code: String,
        message: String,
    },
    Unknown {
        method: String,
        sample: Value,
    },
    ProtocolError {
        message: String,
    },
    /// The owned process ended. `expected` is true when this run asked it to.
    Exited {
        code: Option<i32>,
        reason: Option<String>,
    },
}

/// One protocol connection. Implementations own their reader thread; the run
/// only calls these methods.
pub trait Adapter: Send + Sync {
    fn native_session_id(&self) -> Option<String>;
    /// Sends one user message. Returns once the frame is written, not when the
    /// turn ends.
    fn send_prompt(&self, text: &str) -> Result<(), AppError>;
    /// Asks the CLI to interrupt the current turn. Must not kill the process.
    fn cancel_turn(&self, reason: &str) -> Result<(), AppError>;
    fn answer_permission(
        &self,
        native_request_id: &str,
        option_id: &str,
        kind: &str,
    ) -> Result<(), AppError>;
    /// Closes the connection. Idempotent, never blocks on the reader.
    fn shutdown(&self);
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

struct PermissionSlot {
    view: RuntimePermissionView,
    deadline: Instant,
    answerable: bool,
    /// An answer is being written to the peer right now. `cancel` waits for
    /// this to clear so it can never return while a grant is still committing.
    in_flight: bool,
    /// A deny this client still owes the peer. Set when a request expires or
    /// arrives after its turn was cancelled; the sweeper sends it exactly once.
    obligation: Option<Obligation>,
    /// The adapter reported "submitted but not applied" (CodeBuddy's
    /// `{resolved:false,queued:true}` and its rejection variant). The request
    /// stays answerable; the reason is surfaced instead of pretending success.
    not_applied: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obligation {
    option_id: String,
    reason: String,
}

struct ToolSlot {
    view: RuntimeToolView,
}

struct RunInner {
    execution: ExecutionStatus,
    interaction: InteractionStatus,
    turn: Option<RuntimeTurnView>,
    messages: Vec<RuntimeMessageView>,
    tools: Vec<ToolSlot>,
    permissions: Vec<PermissionSlot>,
    native_session_id: Option<String>,
    error: Option<AppError>,
    notes: Vec<String>,
    ended_at: Option<String>,
    /// Assistant messages whose deltas arrived out of order. Kept separately so
    /// the view can say "this text may have a gap" instead of pretending.
    gapped: Vec<String>,
    journal: EventJournal,
    closed: bool,
    /// Set once the terminal execution state has been written to the Run row.
    /// A process exit and an explicit stop can both reach the end, and only one
    /// of them may own the durable write. Only set **after** a successful
    /// commit, so a failed write can still be retried.
    terminal_persisted: bool,
    /// Turn ids that are over. A completion for one of these must not be
    /// applied to the current turn: on a single connection an old turn can
    /// finish after a new one started, and its id is the only thing that
    /// distinguishes them.
    retired_turns: Vec<String>,
    /// A cancellation was sent but the peer has not confirmed it. The turn is
    /// still running as far as this client knows.
    cancel_pending: bool,
    /// Whether the current turn may still grant permissions. Cleared the moment
    /// a turn is cancelled, ends, or the run stops. Protocols that send no turn
    /// id (Claude, CodeBuddy) have no other way to tell a late request from a
    /// live one, and granting a request that arrived after a confirmed cancel
    /// would reopen exactly the path the cancel closed.
    accepting_permissions: bool,
    /// Events dropped because they came from a previous connection.
    late_events: u64,
    /// Events dropped because the same identity was already applied. Reported
    /// separately: a re-delivery is normal, a foreign connection is not.
    duplicate_events: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StartOutcome {
    pub view: RuntimeRunView,
    pub native_session_id: Option<String>,
}

pub struct RuntimeRun {
    pub run_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub installation_id: Option<String>,
    pub product: super::model::RuntimeProduct,
    pub title: String,
    pub cwd: crate::multicli::domain::HostPath,
    pub started_at: String,
    launch_argv: Mutex<Vec<String>>,
    inner: Mutex<RunInner>,
    adapter: Mutex<Option<Arc<dyn Adapter>>>,
    /// Serialises "write a permission decision" against "cancel the turn".
    /// `answer` holds it across the protocol write; `cancel` must take it
    /// before it marks requests cancelled, so a grant cannot land after cancel
    /// has already returned.
    wire: Mutex<()>,
    process: Mutex<Option<Arc<super::process::OwnedProcess>>>,
    service: Arc<MultiCliService>,
    notify: NotifyFn,
    permission_timeout: Duration,
    generation: u64,
    closed: AtomicBool,
    /// Set by `stop()`: the process exit is then expected and must not be
    /// reported as a crash.
    stop_requested: AtomicBool,
    last_notify_seq: AtomicU64,
    /// Monotonic counter for synthesised dedupe keys.
    fallback_seq: AtomicU64,
    /// Serialises the durable terminal write with every other exit path.
    ///
    /// `stop` used to return while a process-exit finalizer was still blocked on
    /// the SQLite write lock, so the application could quit with the Run row
    /// still `starting` — the same class of race S04 already fixed for
    /// terminals. Whoever finalizes holds this gate until the commit returns.
    finalize_gate: Mutex<()>,
}

impl RuntimeRun {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        service: Arc<MultiCliService>,
        notify: NotifyFn,
        run_id: String,
        session_id: String,
        agent_id: String,
        installation_id: Option<String>,
        product: super::model::RuntimeProduct,
        title: String,
        cwd: crate::multicli::domain::HostPath,
        permission_timeout: Duration,
        generation: u64,
    ) -> Self {
        let mut journal = EventJournal::new(run_id.clone(), session_id.clone());
        let started_at = now_iso();
        journal.push(
            kind::RUN_STATE,
            None,
            "run/starting",
            json!({
                "executionState": ExecutionStatus::Starting.as_str(),
                "interactionState": InteractionStatus::Unknown.as_str(),
            }),
        );
        Self {
            run_id,
            session_id,
            agent_id,
            installation_id,
            product,
            title,
            cwd,
            started_at,
            launch_argv: Mutex::new(Vec::new()),
            inner: Mutex::new(RunInner {
                execution: ExecutionStatus::Starting,
                interaction: InteractionStatus::Unknown,
                turn: None,
                messages: Vec::new(),
                tools: Vec::new(),
                permissions: Vec::new(),
                native_session_id: None,
                error: None,
                notes: Vec::new(),
                ended_at: None,
                gapped: Vec::new(),
                journal,
                closed: false,
                terminal_persisted: false,
                retired_turns: Vec::new(),
                cancel_pending: false,
                accepting_permissions: false,
                late_events: 0,
                duplicate_events: 0,
            }),
            adapter: Mutex::new(None),
            wire: Mutex::new(()),
            process: Mutex::new(None),
            service,
            notify,
            permission_timeout,
            generation,
            closed: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            last_notify_seq: AtomicU64::new(0),
            fallback_seq: AtomicU64::new(0),
            finalize_gate: Mutex::new(()),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn permission_timeout(&self) -> Duration {
        self.permission_timeout
    }

    pub fn attach_adapter(
        &self,
        adapter: Arc<dyn Adapter>,
        process: Arc<super::process::OwnedProcess>,
    ) {
        *self.adapter.lock() = Some(adapter);
        *self.process.lock() = Some(Arc::clone(&process));
        let identity = process.identity.clone();
        {
            let mut inner = self.inner.lock();
            if inner.execution == ExecutionStatus::Starting {
                inner.execution = ExecutionStatus::Running;
            }
            inner.notes.push(format!(
                "pid {} · creationTime {}",
                identity.pid, identity.creation_time
            ));
        }
        if let Err(error) = self.record_owned_process(&process) {
            {
                let mut inner = self.inner.lock();
                inner
                    .notes
                    .push(format!("进程身份未能写入领域库：{}", error.message));
            }
        }
        self.emit_state();
    }

    pub fn set_launch_argv(&self, argv: Vec<String>) {
        *self.launch_argv.lock() = argv;
    }

    fn record_owned_process(
        &self,
        process: &Arc<super::process::OwnedProcess>,
    ) -> Result<(), AppError> {
        let identity = process.identity.clone();
        self.service
            .record_process(&crate::multicli::domain::ProcessObservation {
                identity: identity.clone(),
                parent_identity: None,
                executable: None,
                redacted_command: Some(format!("{} (structured runtime)", self.product.agent_id())),
                observed_at: now_iso(),
            })?;
        self.service
            .link_process(&crate::multicli::domain::RunProcessLink {
                run_id: self.run_id.clone(),
                process_identity: identity,
                link_kind: crate::multicli::domain::LinkKind::Owned,
                evidence: "multicli-runtime-v1: the protocol child was created by this application"
                    .into(),
            })?;
        Ok(())
    }

    pub fn process_identity(&self) -> Option<ProcessIdentity> {
        self.process.lock().as_ref().map(|p| p.identity.clone())
    }

    // -----------------------------------------------------------------------
    // Event sink
    // -----------------------------------------------------------------------

    /// Builds the sink an adapter pushes into. The generation check is what
    /// makes a late event from a previous connection harmless.
    pub fn sink(self: &Arc<Self>) -> super::adapters::SinkFn {
        let weak = Arc::downgrade(self);
        let generation = self.generation;
        Arc::new(move |event: AdapterEvent, meta: EventMeta| {
            let Some(run) = weak.upgrade() else {
                return;
            };
            run.accept(generation, event, meta);
        })
    }

    pub(crate) fn accept(&self, generation: u64, event: AdapterEvent, meta: EventMeta) {
        if generation != self.generation {
            let mut inner = self.inner.lock();
            inner.late_events += 1;
            return;
        }
        if self.closed.load(Ordering::SeqCst) {
            let mut inner = self.inner.lock();
            inner.late_events += 1;
            return;
        }
        let durable = self.apply(event, meta);
        // A run can end because its process exited, which no stop() call
        // observes. Without this the Run row would stay `starting` while the
        // view already said `exited`, and a restart could not reconcile it.
        self.persist_terminal_if_needed();
        self.emit_state();
        for record in durable {
            if let Err(error) = self.persist(&record) {
                let mut inner = self.inner.lock();
                inner.notes.push(format!(
                    "一条 {} 事件未能写入领域库：{}",
                    record.kind, error.message
                ));
            }
        }
    }

    /// Writes the final execution state to the Run row exactly once.
    ///
    /// The whole finalize+commit runs under `finalize_gate`, and
    /// `terminal_persisted` is only set **after** the commit returns. A `stop`
    /// that arrives meanwhile waits on the same gate, so it cannot report
    /// success while the row is still `starting`. A failed commit leaves the
    /// flag clear, so a later exit path can retry instead of being locked out.
    fn persist_terminal_if_needed(&self) {
        let gate = self.finalize_gate.lock();
        let (status, ended) = {
            let inner = self.inner.lock();
            if !inner.execution.is_terminal() || inner.terminal_persisted {
                return;
            }
            (
                inner.execution,
                inner.ended_at.clone().unwrap_or_else(now_iso),
            )
        };
        match self.service.finish_run(
            &self.run_id,
            status,
            Some(&ended),
            Some("multicli-runtime-v1: the run reached a terminal state"),
        ) {
            Ok(()) => {
                let mut inner = self.inner.lock();
                inner.terminal_persisted = true;
                inner.closed = true;
                drop(inner);
                self.closed.store(true, Ordering::SeqCst);
            }
            Err(error) => {
                // Keep the flag clear: the next exit path retries the commit
                // instead of silently leaving the row behind.
                self.inner
                    .lock()
                    .notes
                    .push(format!("终态未能写入领域库，将重试：{}", error.message));
                eprintln!("[multicli] failed to close run {}: {error}", self.run_id);
            }
        }
        drop(gate);
    }

    fn emit_state(&self) {
        let (seq, execution, interaction) = {
            let inner = self.inner.lock();
            (
                inner.journal.last_seq(),
                inner.execution.as_str().to_string(),
                inner.interaction.as_str().to_string(),
            )
        };
        let previous = self.last_notify_seq.swap(seq, Ordering::SeqCst);
        if previous == seq && seq != 0 {
            return;
        }
        (self.notify)(RuntimeNotify {
            run_id: self.run_id.clone(),
            seq,
            kind: "event".into(),
            interaction_state: interaction,
            execution_state: execution,
        });
    }

    /// Applies one event and returns the records that must be persisted.
    ///
    /// Every payload and key is built in its own statement before the journal
    /// is touched: `EventJournal::push` needs `&mut inner`, so a payload that
    /// reads `inner.execution` inside the same expression would borrow `inner`
    /// twice.
    fn apply(&self, event: AdapterEvent, meta: EventMeta) -> Vec<PersistRecord> {
        let mut out = Vec::new();
        let mut inner = self.inner.lock();
        // Dedupe **before** anything is mutated. The journal already refused to
        // append a duplicate, but by then a replayed `turn/started` had already
        // reopened a completed turn in memory. A duplicate now changes nothing.
        let source_key = meta.source_key.clone().unwrap_or_default();
        if inner
            .journal
            .is_duplicate(meta.native_event_id.as_deref(), &source_key)
        {
            inner.duplicate_events += 1;
            return out;
        }
        match event {
            AdapterEvent::SessionBound { native_session_id } => {
                inner.native_session_id = Some(native_session_id.clone());
                let execution = inner.execution.as_str();
                let interaction = inner.interaction.as_str();
                let payload = json!({
                    "executionState": execution,
                    "interactionState": interaction,
                    "nativeSessionId": native_session_id,
                });
                let key = self.key(&meta, "session-bound");
                inner.journal.push(
                    kind::RUN_STATE,
                    meta.native_event_id.as_deref(),
                    &key,
                    payload,
                );
            }
            AdapterEvent::TurnStarted {
                turn_id,
                model,
                effort,
            } => {
                if inner.retired_turns.iter().any(|id| id == &turn_id) {
                    // This turn already ended; the start is a late or replayed
                    // frame. It must not restart it.
                    inner
                        .notes
                        .push(format!("忽略已结束回合 {turn_id} 的重复开始事件"));
                    return out;
                }
                let attaches = inner
                    .turn
                    .as_ref()
                    .is_some_and(|turn| turn.state == "running" && turn.turn_id == turn_id);
                let adopts_provisional = inner.turn.as_ref().is_some_and(|turn| {
                    turn.state == "running" && turn.turn_id.starts_with("turn-")
                });
                if attaches || adopts_provisional {
                    // The product named the turn the user already started (or
                    // repeated its own name). Adopt the native id so a later
                    // completion can be matched exactly.
                    if let Some(turn) = inner.turn.as_mut() {
                        turn.turn_id = turn_id.clone();
                        if model.is_some() {
                            turn.model = model.clone();
                        }
                        if effort.is_some() {
                            turn.effort = effort.clone();
                        }
                    }
                } else {
                    if let Some(previous) = inner.turn.take() {
                        if !inner.retired_turns.contains(&previous.turn_id) {
                            inner.retired_turns.push(previous.turn_id);
                        }
                    }
                    inner.turn = Some(RuntimeTurnView {
                        turn_id: turn_id.clone(),
                        state: "running".into(),
                        started_at: now_iso(),
                        ended_at: None,
                        model: model.clone(),
                        effort: effort.clone(),
                        reason: None,
                    });
                }
                inner.accepting_permissions = true;
                if inner.retired_turns.len() > 32 {
                    let drop = inner.retired_turns.len() - 32;
                    inner.retired_turns.drain(..drop);
                }
                inner.interaction = InteractionStatus::Working;
                let payload = json!({
                    "turnId": turn_id,
                    "model": model,
                    "effort": effort,
                });
                let key = self.key(&meta, &format!("turn-start:{turn_id}"));
                inner.journal.push(
                    kind::TURN_STARTED,
                    meta.native_event_id.as_deref(),
                    &key,
                    payload,
                );
                out.push(self.durable(
                    &inner,
                    kind::TURN_STARTED,
                    json!({ "turnId": turn_id, "model": model, "effort": effort }),
                    meta.native_event_id.clone(),
                    format!("turn-start:{turn_id}"),
                ));
            }
            AdapterEvent::TextDelta {
                message_id,
                index,
                text,
            } => {
                if text.is_empty() {
                    return out;
                }
                let seq = inner.journal.last_seq();
                let message = ensure_message(&mut inner.messages, &message_id, seq);
                let (applied, gap) = append_delta(message, index, &text);
                if let Some(expected) = gap {
                    if !inner.gapped.contains(&message_id) {
                        inner.gapped.push(message_id.clone());
                    }
                    inner.notes.push(format!(
                        "消息 {message_id} 的增量出现缺口：期望偏移 {expected}，实到 {index}"
                    ));
                }
                if applied.is_empty() {
                    return out;
                }
                inner.interaction = InteractionStatus::Working;
                let payload = json!({
                    "messageId": message_id,
                    "role": "assistant",
                    "index": index,
                    "text": applied,
                });
                let key = self.key(&meta, &format!("delta:{message_id}:{index}"));
                inner.journal.push(
                    kind::MESSAGE_DELTA,
                    meta.native_event_id.as_deref(),
                    &key,
                    payload,
                );
            }
            AdapterEvent::ReasoningDelta {
                message_id,
                index,
                text,
            } => {
                if text.is_empty() {
                    return out;
                }
                inner.interaction = InteractionStatus::Working;
                let payload = json!({ "messageId": message_id, "index": index, "text": text });
                let key = self.key(&meta, &format!("reason:{message_id}:{index}"));
                inner.journal.push(
                    kind::REASONING_DELTA,
                    meta.native_event_id.as_deref(),
                    &key,
                    payload,
                );
            }
            AdapterEvent::TextCompleted {
                message_id,
                role,
                text,
            } => {
                match inner
                    .messages
                    .iter_mut()
                    .find(|message| message.message_id == message_id)
                {
                    Some(message) => {
                        // The final text is authoritative: it replaces the
                        // accumulated deltas, which is what makes a re-delivered
                        // or partially missed delta harmless.
                        message.text = text.clone();
                        message.complete = true;
                        message.role = role.clone();
                    }
                    None => {
                        let seq = inner.journal.last_seq();
                        inner.messages.push(RuntimeMessageView {
                            message_id: message_id.clone(),
                            role: role.clone(),
                            text: text.clone(),
                            complete: true,
                            seq,
                        });
                    }
                }
                inner.gapped.retain(|id| id != &message_id);
                let payload = json!({
                    "messageId": message_id,
                    "role": role,
                    "text": text,
                });
                let key = self.key(&meta, &format!("message:{message_id}"));
                inner.journal.push(
                    kind::MESSAGE_COMPLETED,
                    meta.native_event_id.as_deref(),
                    &key,
                    payload,
                );
                out.push(self.durable(
                    &inner,
                    kind::MESSAGE_COMPLETED,
                    json!({ "messageId": message_id, "role": role, "text": text }),
                    meta.native_event_id.clone(),
                    format!("message:{message_id}"),
                ));
            }
            AdapterEvent::ToolCall {
                tool_call_id,
                name,
                kind: tool_kind,
                input,
                permission_id,
            } => {
                // A tool call that already has a permission request keeps its
                // authorization state; linking is by `toolCallId`, never by
                // arrival order. A call with no request stays `not-requested`,
                // which is a fact about this run and not a denial.
                let known = inner
                    .permissions
                    .iter()
                    .find(|slot| slot.view.tool_call_id.as_deref() == Some(tool_call_id.as_str()))
                    .map(|slot| {
                        (
                            slot.view.permission_id.clone(),
                            authorization_for_view(&slot.view),
                        )
                    });
                let (linked_permission, authorization) = match known {
                    Some((id, state)) => (Some(id), state),
                    None => (permission_id.clone(), "not-requested".to_string()),
                };
                let slot = RuntimeToolView {
                    tool_call_id: tool_call_id.clone(),
                    permission_id: linked_permission,
                    authorization,
                    execution: "running".into(),
                    outcome: "unknown".into(),
                    name: name.clone(),
                    kind: tool_kind.clone(),
                    status: "running".into(),
                    input: input.clone(),
                    output: None,
                    is_error: false,
                    started_at: now_iso(),
                    ended_at: None,
                };
                if let Some(existing) = inner
                    .tools
                    .iter_mut()
                    .find(|tool| tool.view.tool_call_id == tool_call_id)
                {
                    existing.view = slot;
                } else {
                    inner.tools.push(ToolSlot { view: slot });
                }
                inner.interaction = InteractionStatus::Working;
                let payload = json!({
                    "toolCallId": tool_call_id,
                    "permissionId": permission_id,
                    "name": name,
                    "kind": tool_kind,
                    "input": input,
                    "status": "running",
                });
                let key = self.key(&meta, &format!("tool:{tool_call_id}"));
                inner.journal.push(
                    kind::TOOL_CALL,
                    meta.native_event_id.as_deref(),
                    &key,
                    payload,
                );
                out.push(self.durable(
                    &inner,
                    kind::TOOL_CALL,
                    json!({
                        "toolCallId": tool_call_id,
                        "name": name,
                        "kind": tool_kind,
                        "input": input,
                        "permissionId": permission_id,
                    }),
                    meta.native_event_id.clone(),
                    format!("tool:{tool_call_id}"),
                ));
            }
            AdapterEvent::ToolUpdate {
                tool_call_id,
                status,
                output,
                is_error,
            } => {
                // Denial is a fact about **this client's** decision, so it comes
                // from the outbound ledger — never from `is_error`, never from a
                // literal in the text. A successful read whose document happens
                // to contain a marker string must stay `executed`.
                let linked = inner
                    .permissions
                    .iter()
                    .find(|slot| slot.view.tool_call_id.as_deref() == Some(tool_call_id.as_str()))
                    .map(|slot| slot.view.clone());
                let ledger_authorization = linked
                    .as_ref()
                    .map(authorization_for_view)
                    .unwrap_or_else(|| "not-requested".into());
                let ledger_denied =
                    matches!(ledger_authorization.as_str(), "denied" | "auto-denied");
                let ledger_answered = linked.as_ref().is_some_and(|view| view.state == "answered");
                // Tool output is data, never a verdict about this client's
                // protocol frames: a file being read may legitimately start with
                // any string, and treating that as a contract error killed the
                // whole session. Protocol failures arrive as error frames.
                let _ = (ledger_answered, ledger_denied);
                // A denial recorded in the ledger outranks whatever the protocol
                // reported: the tool was refused, so it did not run.
                let effective_status = if ledger_denied {
                    "denied".to_string()
                } else {
                    status.clone()
                };
                let mut placeholder = false;
                if let Some(existing) = inner
                    .tools
                    .iter_mut()
                    .find(|tool| tool.view.tool_call_id == tool_call_id)
                {
                    existing.view.status = effective_status.clone();
                    existing.view.execution = execution_for(&effective_status, output.is_some());
                    existing.view.outcome = outcome_for(&effective_status, is_error);
                    if ledger_authorization != "not-requested" {
                        existing.view.authorization = ledger_authorization.clone();
                    }
                    if output.is_some() {
                        existing.view.output = output.clone();
                    }
                    existing.view.is_error = is_error;
                    if matches!(
                        status.as_str(),
                        "completed" | "failed" | "denied" | "cancelled"
                    ) {
                        existing.view.ended_at = Some(now_iso());
                    }
                } else {
                    // An update for an unknown call is preserved instead of
                    // dropped: the transcript would otherwise show a result
                    // with no call.
                    placeholder = true;
                    let ended = now_iso();
                    inner.tools.push(ToolSlot {
                        view: RuntimeToolView {
                            tool_call_id: tool_call_id.clone(),
                            permission_id: linked.as_ref().map(|view| view.permission_id.clone()),
                            authorization: ledger_authorization.clone(),
                            execution: execution_for(&effective_status, output.is_some()),
                            outcome: outcome_for(&status, is_error),
                            name: "unknown".into(),
                            kind: "other".into(),
                            status: effective_status.clone(),
                            input: None,
                            output: output.clone(),
                            is_error,
                            started_at: ended.clone(),
                            ended_at: Some(ended),
                        },
                    });
                }
                if placeholder {
                    inner.notes.push(format!(
                        "工具 {tool_call_id} 的结果早于调用事件到达，已按结果补建记录"
                    ));
                }
                let payload = json!({
                    "toolCallId": tool_call_id,
                    "status": status,
                    "output": output,
                    "isError": is_error,
                });
                let key = self.key(&meta, &format!("tool-update:{tool_call_id}:{status}"));
                inner.journal.push(
                    kind::TOOL_UPDATE,
                    meta.native_event_id.as_deref(),
                    &key,
                    payload,
                );
                out.push(self.durable(
                    &inner,
                    kind::TOOL_UPDATE,
                    json!({
                        "toolCallId": tool_call_id,
                        "status": status,
                        "output": output,
                        "isError": is_error,
                    }),
                    meta.native_event_id.clone(),
                    format!("tool-update:{tool_call_id}:{status}"),
                ));
            }
            AdapterEvent::PermissionRequested {
                native_request_id,
                title,
                detail,
                kind: permission_kind,
                options,
                tool_call_id,
                turn_id,
            } => {
                let permission_id = format!("perm-{}", uuid::Uuid::new_v4().simple());
                // Ownership gate. A request can arrive *after* the user
                // cancelled the turn (or after it ended): opening an answerable
                // card would let a cancelled operation be granted retroactively.
                // A confirmed cancel clears `cancel_pending`, so it cannot be
                // the only signal: an old turn's request may arrive afterwards.
                let nothing_to_grant = !inner.accepting_permissions;
                let for_retired_turn = turn_id
                    .as_ref()
                    .is_some_and(|id| inner.retired_turns.iter().any(|known| known == id));
                let current_turn = inner.turn.as_ref().map(|turn| turn.turn_id.clone());
                let for_other_turn = match (&turn_id, &current_turn) {
                    (Some(id), Some(current)) => id != current,
                    _ => false,
                };
                if nothing_to_grant || inner.cancel_pending || for_retired_turn || for_other_turn {
                    let deny_option = options
                        .iter()
                        .find(|option| option.kind.starts_with("deny"))
                        .or_else(|| options.iter().find(|option| option.kind == "cancel"))
                        .map(|option| option.option_id.clone());
                    let reason = if nothing_to_grant {
                        "当前没有可授权的回合（已取消或已结束），迟到的权限请求作废"
                    } else if inner.cancel_pending {
                        "回合正在取消，迟到的权限请求作废；不会打开新的授权路径"
                    } else {
                        "该权限请求所属的回合已结束，请求作废"
                    };
                    let view = RuntimePermissionView {
                        permission_id: permission_id.clone(),
                        native_request_id: Some(native_request_id),
                        tool_call_id: tool_call_id.clone(),
                        title: title.clone(),
                        detail: detail.clone(),
                        kind: permission_kind.clone(),
                        options: options.clone(),
                        state: "cancelled".into(),
                        selected_option_id: None,
                        auto_denied: true,
                        reason: Some(reason.to_string()),
                        turn_id: turn_id.clone(),
                        created_at: now_iso(),
                        resolved_at: Some(now_iso()),
                    };
                    inner.permissions.push(PermissionSlot {
                        view: view.clone(),
                        deadline: Instant::now() + self.permission_timeout,
                        answerable: false,
                        in_flight: false,
                        // The peer is still waiting: we owe it a deny. The
                        // sweeper sends it once, off the state lock.
                        obligation: deny_option.map(|option_id| Obligation {
                            option_id,
                            reason: reason.to_string(),
                        }),
                        not_applied: None,
                    });
                    let payload = serde_json::to_value(&view).unwrap_or_else(|_| json!({}));
                    let key = self.key(&meta, &format!("permission-void:{permission_id}"));
                    inner.journal.push(
                        kind::PERMISSION_REQUESTED,
                        meta.native_event_id.as_deref(),
                        &key,
                        payload.clone(),
                    );
                    out.push(self.durable(
                        &inner,
                        kind::PERMISSION_REQUESTED,
                        payload,
                        meta.native_event_id.clone(),
                        format!("permission-void:{permission_id}"),
                    ));
                    return out;
                }
                let view = RuntimePermissionView {
                    permission_id: permission_id.clone(),
                    native_request_id: Some(native_request_id),
                    tool_call_id: tool_call_id.clone(),
                    title: title.clone(),
                    detail: detail.clone(),
                    kind: permission_kind.clone(),
                    options: options.clone(),
                    state: "pending".into(),
                    selected_option_id: None,
                    auto_denied: false,
                    reason: None,
                    turn_id: turn_id.clone(),
                    created_at: now_iso(),
                    resolved_at: None,
                };
                // The request may arrive after the tool call (Codex) or before
                // it (Claude): both directions are linked by `toolCallId`.
                if let Some(tool_call_id) = tool_call_id.as_deref() {
                    if let Some(tool) = inner
                        .tools
                        .iter_mut()
                        .find(|tool| tool.view.tool_call_id == tool_call_id)
                    {
                        tool.view.permission_id = Some(permission_id.clone());
                        tool.view.authorization = "pending".into();
                    }
                }
                inner.permissions.push(PermissionSlot {
                    view: view.clone(),
                    deadline: Instant::now() + self.permission_timeout,
                    answerable: true,
                    in_flight: false,
                    obligation: None,
                    not_applied: None,
                });
                inner.interaction = InteractionStatus::AwaitingPermission;
                let payload = serde_json::to_value(&view).unwrap_or_else(|_| json!({}));
                let key = self.key(&meta, &format!("permission:{permission_id}"));
                inner.journal.push(
                    kind::PERMISSION_REQUESTED,
                    meta.native_event_id.as_deref(),
                    &key,
                    payload.clone(),
                );
                out.push(self.durable(
                    &inner,
                    kind::PERMISSION_REQUESTED,
                    payload,
                    meta.native_event_id.clone(),
                    format!("permission:{permission_id}"),
                ));
            }
            AdapterEvent::PermissionResolved {
                native_request_id,
                option_id,
                state,
            } => {
                let resolved = {
                    let slot = inner.permissions.iter_mut().find(|slot| {
                        slot.view.native_request_id.as_deref() == Some(native_request_id.as_str())
                            && slot.view.state == "pending"
                    });
                    slot.map(|slot| {
                        slot.view.state = state.clone();
                        slot.view.selected_option_id = option_id.clone();
                        slot.view.resolved_at = Some(now_iso());
                        slot.answerable = false;
                        slot.view.clone()
                    })
                };
                if let Some(view) = resolved {
                    if let Some(tool_call_id) = view.tool_call_id.as_deref() {
                        // The decision is read from the option the user
                        // actually picked, so "always allow" is not recorded as
                        // a one-shot allow.
                        let authorization = authorization_for_view(&view);
                        if let Some(tool) = inner
                            .tools
                            .iter_mut()
                            .find(|tool| tool.view.tool_call_id == tool_call_id)
                        {
                            tool.view.permission_id = Some(view.permission_id.clone());
                            tool.view.authorization = authorization;
                        }
                    }
                    if !inner
                        .permissions
                        .iter()
                        .any(|other| other.view.state == "pending")
                    {
                        inner.interaction = if inner.turn.is_some() {
                            InteractionStatus::Working
                        } else {
                            InteractionStatus::Idle
                        };
                    }
                    let payload = serde_json::to_value(&view).unwrap_or_else(|_| json!({}));
                    let key = self.key(&meta, &format!("permission-resolved:{native_request_id}"));
                    inner.journal.push(
                        kind::PERMISSION_RESOLVED,
                        meta.native_event_id.as_deref(),
                        &key,
                        payload.clone(),
                    );
                    out.push(self.durable(
                        &inner,
                        kind::PERMISSION_RESOLVED,
                        payload,
                        meta.native_event_id.clone(),
                        format!("permission-resolved:{native_request_id}"),
                    ));
                }
            }
            AdapterEvent::PermissionNotApplied {
                native_request_id,
                state,
                detail,
            } => {
                let mut touched = None;
                for slot in inner.permissions.iter_mut() {
                    if slot.view.native_request_id.as_deref() == Some(native_request_id.as_str())
                        && slot.view.state == "pending"
                    {
                        slot.not_applied = Some(state.clone());
                        // Still answerable: nothing was applied, so the user
                        // may retry a different option or cancel the turn.
                        slot.answerable = true;
                        slot.view.reason = Some(match state.as_str() {
                            "rejected" => {
                                "CodeBuddy 拒绝应用该决定；请求仍待答复，可重试或取消".into()
                            }
                            _ => "CodeBuddy 已登记但尚未应用该决定；请求仍待答复，可重试或取消"
                                .into(),
                        });
                        touched = Some(slot.view.clone());
                        break;
                    }
                }
                if let Some(view) = touched {
                    inner.notes.push(format!(
                        "权限决定未应用（{state}）：{}",
                        detail.unwrap_or_else(|| "请求保持待答复".into())
                    ));
                    let key = format!("permission-not-applied:{}", view.permission_id);
                    inner.journal.push(
                        kind::PERMISSION_RESOLVED,
                        meta.native_event_id.as_deref(),
                        &key,
                        serde_json::to_value(&view).unwrap_or_else(|_| json!({})),
                    );
                }
            }
            AdapterEvent::TurnCompleted {
                turn_id,
                status,
                reason,
            } => {
                // Ownership check first. On one connection an old turn can
                // finish after a new one started, and adopting "the current turn
                // is running" as proof would let the old completion rename and
                // close the new turn.
                let is_current = inner
                    .turn
                    .as_ref()
                    .is_some_and(|turn| turn.turn_id == turn_id);
                let is_retired = inner.retired_turns.iter().any(|id| id == &turn_id);
                let provisional = inner.turn.as_ref().is_some_and(|turn| {
                    turn.state == "running" && turn.turn_id.starts_with("turn-")
                });
                if is_retired || (!is_current && !provisional) {
                    // Recorded, so a report can explain it, but it changes no
                    // state: the current turn and its permissions stay intact.
                    let payload = json!({
                        "turnId": turn_id,
                        "status": status,
                        "applied": false,
                        "reason": "not the current turn",
                    });
                    let key = self.key(&meta, &format!("turn-end-late:{turn_id}"));
                    inner.journal.push(
                        kind::TURN_COMPLETED,
                        meta.native_event_id.as_deref(),
                        &key,
                        payload,
                    );
                    inner.notes.push(format!(
                        "忽略回合 {turn_id} 的结束事件：它不属于当前回合（{status}）"
                    ));
                    return out;
                }
                // A turn that ended cancels everything still waiting on it: an
                // answer can never land on a finished turn.
                let cancelled: Vec<RuntimePermissionView> = {
                    let mut collected = Vec::new();
                    for slot in inner.permissions.iter_mut() {
                        let belongs = slot.view.turn_id.as_deref().is_none_or(|id| id == turn_id);
                        if belongs {
                            // A confirmed terminal turn no longer waits for its
                            // old requests; do not retry already-cleared ids.
                            slot.obligation = None;
                        }
                        if slot.view.state == "pending" && belongs {
                            slot.view.state = "cancelled".into();
                            slot.view.reason =
                                Some("该轮次已结束，未答复的权限请求不再有效".into());
                            slot.view.resolved_at = Some(now_iso());
                            slot.answerable = false;
                            collected.push(slot.view.clone());
                        }
                    }
                    collected
                };
                for view in &cancelled {
                    if let Some(tool_call_id) = view.tool_call_id.as_deref() {
                        if let Some(tool) = inner
                            .tools
                            .iter_mut()
                            .find(|tool| tool.view.tool_call_id == tool_call_id)
                        {
                            tool.view.permission_id = Some(view.permission_id.clone());
                            tool.view.authorization = authorization_for_view(view);
                        }
                    }
                    let payload = serde_json::to_value(view).unwrap_or_else(|_| json!({}));
                    let key = format!("permission-cancelled:{}", view.permission_id);
                    inner
                        .journal
                        .push(kind::PERMISSION_RESOLVED, None, &key, payload.clone());
                    out.push(self.durable(&inner, kind::PERMISSION_RESOLVED, payload, None, key));
                }
                if let Some(turn) = inner.turn.as_mut() {
                    // The turn reached here because it is either the current one
                    // or a provisional turn the product has now named.
                    turn.turn_id = turn_id.clone();
                    turn.state = status.clone();
                    turn.ended_at = Some(now_iso());
                    turn.reason = reason.clone();
                }
                if !inner.retired_turns.contains(&turn_id) {
                    inner.retired_turns.push(turn_id.clone());
                }
                if inner.retired_turns.len() > 32 {
                    let drop = inner.retired_turns.len() - 32;
                    inner.retired_turns.drain(..drop);
                }
                // The peer spoke: whatever we were waiting to hear about a
                // cancellation has arrived, and nothing from that turn may be
                // granted any more.
                inner.cancel_pending = false;
                inner.accepting_permissions = false;
                // Any assistant message still open is closed here: a turn that
                // ended cancelled must not leave a message spinning forever.
                for message in inner.messages.iter_mut() {
                    if !message.complete {
                        message.complete = true;
                    }
                }
                // A tool that never reported a result is a real risk: the user
                // may have allowed it and the CLI died before saying anything.
                // It becomes an explicit `unknown` instead of a permanent
                // "running" the UI would wait on forever.
                let mut dangling = 0_usize;
                for tool in inner.tools.iter_mut() {
                    if tool.view.execution == "running" {
                        tool.view.execution = "unknown".into();
                        tool.view.outcome = "unknown".into();
                        tool.view.status = "unknown".into();
                        tool.view.ended_at = Some(now_iso());
                        dangling += 1;
                    }
                }
                if dangling > 0 {
                    inner.notes.push(format!(
                        "{dangling} 个工具调用在本轮结束时没有结果；已标记为未知，不作为执行成功"
                    ));
                }
                inner.interaction = if inner
                    .permissions
                    .iter()
                    .any(|slot| slot.view.state == "pending")
                {
                    InteractionStatus::AwaitingPermission
                } else {
                    InteractionStatus::Idle
                };
                let payload = json!({
                    "turnId": turn_id,
                    "status": status,
                    "reason": reason,
                });
                let key = self.key(&meta, &format!("turn-end:{turn_id}"));
                inner.journal.push(
                    kind::TURN_COMPLETED,
                    meta.native_event_id.as_deref(),
                    &key,
                    payload,
                );
                out.push(self.durable(
                    &inner,
                    kind::TURN_COMPLETED,
                    json!({ "turnId": turn_id, "status": status, "reason": reason }),
                    meta.native_event_id.clone(),
                    format!("turn-end:{turn_id}"),
                ));
            }
            AdapterEvent::Warning { code, message } => {
                inner.notes.push(format!("{code}: {message}"));
                let payload = json!({ "code": code, "message": message });
                let key = self.key(&meta, &format!("warning:{code}"));
                inner.journal.push(
                    kind::WARNING,
                    meta.native_event_id.as_deref(),
                    &key,
                    payload,
                );
            }
            AdapterEvent::Unknown { method, sample } => {
                let key = self.key(&meta, &format!("unknown:{method}"));
                inner
                    .journal
                    .push_unknown(&method, meta.native_event_id.as_deref(), &key, sample);
            }
            AdapterEvent::ProtocolError { message } => {
                let error = AppError::new(ErrorCode::Protocol, message.clone())
                    .with_details(json!({ "stage": "protocol", "runId": self.run_id }));
                inner.error = Some(error);
                inner.interaction = InteractionStatus::Unknown;
                let payload = json!({ "code": "protocolError", "message": message });
                let key = self.key(&meta, "protocol-error");
                inner.journal.push(
                    kind::WARNING,
                    meta.native_event_id.as_deref(),
                    &key,
                    payload,
                );
            }
            AdapterEvent::Exited { code, reason } => {
                invalidate_run_permissions(&mut inner, "协议连接已结束，旧权限请求失效");
                inner.execution = if self.stop_requested.load(Ordering::SeqCst) {
                    ExecutionStatus::Exited
                } else if code == Some(0) || code.is_none() {
                    ExecutionStatus::Exited
                } else {
                    ExecutionStatus::Failed
                };
                inner.ended_at = Some(now_iso());
                inner.interaction = InteractionStatus::Unknown;
                if let Some(reason) = &reason {
                    inner.notes.push(reason.clone());
                }
                let execution = inner.execution.as_str();
                let interaction = inner.interaction.as_str();
                let payload = json!({
                    "executionState": execution,
                    "interactionState": interaction,
                    "exitCode": code,
                    "reason": reason,
                });
                let key = self.key(&meta, "exited");
                inner.journal.push(
                    kind::RUN_STATE,
                    meta.native_event_id.as_deref(),
                    &key,
                    payload,
                );
                out.push(self.durable(
                    &inner,
                    kind::RUN_STATE,
                    json!({ "executionState": execution, "exitCode": code }),
                    meta.native_event_id.clone(),
                    "run-exited".into(),
                ));
            }
        }
        out
    }

    fn key(&self, meta: &EventMeta, fallback: &str) -> String {
        if let Some(key) = &meta.source_key {
            return key.clone();
        }
        // The counter lives on the run, not in the state it protects, so a
        // fallback key can be built without borrowing the journal.
        format!(
            "{fallback}#{}",
            self.fallback_seq.fetch_add(1, Ordering::Relaxed) + 1
        )
    }

    // -----------------------------------------------------------------------
    // Operations
    // -----------------------------------------------------------------------

    pub fn send(&self, text: &str) -> Result<RuntimeRunView, AppError> {
        // One outbound operation at a time. A second send must see the turn
        // reserved by the first, even when the transport is slow.
        let _wire = self.wire.lock();
        {
            let inner = self.inner.lock();
            if inner.closed || inner.execution.is_terminal() {
                return Err(AppError::conflict("run", "this run has already ended")
                    .with_details(json!({ "runId": self.run_id })));
            }
            if matches!(
                inner.interaction,
                InteractionStatus::AwaitingPermission | InteractionStatus::AwaitingInput
            ) {
                return Err(AppError::conflict(
                    "run",
                    "answer or cancel the pending request before sending another message",
                )
                .with_details(json!({
                    "runId": self.run_id,
                    "interactionState": inner.interaction.as_str(),
                })));
            }
            let running = inner
                .turn
                .as_ref()
                .is_some_and(|turn| turn.state == "running");
            // A running turn blocks a new message, unless the user already
            // cancelled it and the peer simply has not confirmed: then moving on
            // is an explicit choice, and the old turn is retired below so a late
            // completion cannot touch the new one.
            if running && !inner.cancel_pending {
                return Err(AppError::conflict(
                    "run",
                    "a turn is already running; wait for it or cancel it first",
                )
                .with_details(json!({ "runId": self.run_id })));
            }
        }
        let adapter =
            self.adapter.lock().clone().ok_or_else(|| {
                AppError::conflict("run", "the protocol connection is not attached")
            })?;
        let provisional_id = format!("turn-{}", uuid::Uuid::new_v4().simple());
        let user_record = {
            let mut inner = self.inner.lock();
            let seq = inner.journal.last_seq();
            let message_id = format!("user-{}", uuid::Uuid::new_v4().simple());
            inner.messages.push(RuntimeMessageView {
                message_id: message_id.clone(),
                role: "user".into(),
                text: text.to_string(),
                complete: true,
                seq,
            });
            // A turn starts when the user sends, not when a product happens to
            // announce it: Claude Code sends nothing at the start of a turn.
            // The id is provisional until the CLI names one.
            if let Some(previous) = inner.turn.take() {
                if !inner.retired_turns.contains(&previous.turn_id) {
                    inner.retired_turns.push(previous.turn_id);
                }
                if previous.state == "running" {
                    inner.notes.push(
                        "上一个回合未被确认取消，已开始新回合；旧回合的晚到事件将被忽略"
                            .to_string(),
                    );
                }
            }
            if inner.retired_turns.len() > 32 {
                let drop = inner.retired_turns.len() - 32;
                inner.retired_turns.drain(..drop);
            }
            inner.cancel_pending = false;
            // A new message opens a new turn: its permissions are grantable
            // until that turn is cancelled or ends.
            inner.accepting_permissions = true;
            inner.turn = Some(RuntimeTurnView {
                turn_id: provisional_id.clone(),
                state: "running".into(),
                started_at: now_iso(),
                ended_at: None,
                model: None,
                effort: None,
                reason: None,
            });
            inner.interaction = InteractionStatus::Working;
            let key = self.key(&EventMeta::default(), "user");
            let payload = json!({ "messageId": message_id, "role": "user", "text": text });
            inner
                .journal
                .push(kind::MESSAGE_COMPLETED, None, &key, payload.clone());
            self.durable(&inner, kind::MESSAGE_COMPLETED, payload, None, key)
        };
        if let Err(error) = self.persist(&user_record) {
            self.note(format!("用户消息未能写入领域库：{}", error.message));
        }
        if let Err(error) = adapter.send_prompt(text) {
            let mut inner = self.inner.lock();
            let unsent = answer_failure_is_definitely_unsent(&error);
            if !inner.closed && !inner.execution.is_terminal() {
                if unsent {
                    if let Some(turn) = inner.turn.as_mut().filter(|turn| turn.state == "running") {
                        turn.state = "failed".into();
                        turn.ended_at = Some(now_iso());
                        turn.reason = Some(error.message.clone());
                    }
                    inner.accepting_permissions = false;
                    inner.interaction = InteractionStatus::Idle;
                } else {
                    // A partial write may already be executing. Keep the turn
                    // occupied until a completion or explicit stop settles it.
                    inner.interaction = InteractionStatus::Unknown;
                }
                inner.notes.push(format!(
                    "消息发送失败（{}）：{}",
                    if unsent { "未写入" } else { "结果未知" },
                    error.message
                ));
                let key = self.key(&EventMeta::default(), "prompt-send-failed");
                inner.journal.push(kind::WARNING, None, &key, json!({"code":"promptSendFailed","definitelyUnsent":unsent,"message":error.message}));
            }
            drop(inner);
            self.emit_state();
            return Err(error);
        }
        Ok(self.view())
    }

    /// Cancels the current turn. Pending permissions of that turn stop being
    /// answerable *before* the protocol is asked, so a late approval can never
    /// win the race.
    /// Asks the CLI to interrupt the current turn.
    ///
    /// "The request was sent" and "the turn stopped" are different facts. The
    /// turn is **not** marked cancelled here: it is marked when the peer says
    /// so. Until then the interaction state stays `unknown`, because claiming
    /// `idle` would tell the user the turn is over while it may still be
    /// writing.
    pub fn cancel(&self, reason: &str, grace: Duration) -> Result<RuntimeRunView, AppError> {
        let (running, any_pending) = {
            let inner = self.inner.lock();
            (
                inner
                    .turn
                    .as_ref()
                    .is_some_and(|turn| turn.state == "running"),
                inner
                    .permissions
                    .iter()
                    .any(|slot| slot.view.state == "pending"),
            )
        };
        if !running && !any_pending {
            return Err(
                AppError::conflict("run", "there is no running turn to cancel")
                    .with_details(json!({ "runId": self.run_id })),
            );
        }
        // Close the grant path first: no new request may become answerable, and
        // `answer` refuses while `cancel_pending` is set. Both flags go down
        // before waiting, so the wait cannot be outrun by a fresh approval.
        {
            let mut inner = self.inner.lock();
            inner.cancel_pending = true;
            inner.accepting_permissions = false;
            inner.interaction = InteractionStatus::Unknown;
        }

        // A decision already on the wire gets to finish first. This is real
        // serialisation, not a poll: `answer` holds `wire` across its protocol
        // write, so acquiring it here means either that write has completed or
        // it has not started. A timeout is an error — never a silent success.
        let wire = {
            let deadline = Instant::now() + Duration::from_secs(35);
            loop {
                if let Some(guard) = self.wire.try_lock() {
                    break guard;
                }
                if Instant::now() >= deadline {
                    return Err(AppError::conflict(
                        "run",
                        "a permission answer is still being written; the turn was not cancelled, retry after it settles",
                    )
                    .with_details(json!({ "runId": self.run_id, "stage": "cancelWire" }))
                    .with_retryable(true));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        };

        let (had_work, cancelled_views) = {
            let mut inner = self.inner.lock();
            // A cancellation also invalidates every request still waiting: no
            // answer may land on a turn the user just abandoned.
            let mut collected = Vec::new();
            for slot in inner.permissions.iter_mut() {
                if slot.view.state == "pending" {
                    slot.view.state = "cancelled".into();
                    slot.view.reason = Some(reason.to_string());
                    slot.view.resolved_at = Some(now_iso());
                    slot.answerable = false;
                    // The peer is still blocked on this request: the deny it is
                    // owed is scheduled through the same convergence path.
                    slot.obligation =
                        deny_option_of(&slot.view.options).map(|option_id| Obligation {
                            option_id,
                            reason: "回合被取消，按拒绝收敛对端等待".into(),
                        });
                    collected.push(slot.view.clone());
                }
            }
            (running, collected)
        };
        // Everything still pending is now cancelled, and no new request can be
        // granted, so a fresh answer cannot race the interrupt below.
        for view in &cancelled_views {
            if let Some(tool_call_id) = view.tool_call_id.as_deref() {
                let mut inner = self.inner.lock();
                if let Some(tool) = inner
                    .tools
                    .iter_mut()
                    .find(|tool| tool.view.tool_call_id == tool_call_id)
                {
                    tool.view.permission_id = Some(view.permission_id.clone());
                    tool.view.authorization = authorization_for_view(view);
                }
            }
            let payload = serde_json::to_value(view).unwrap_or_else(|_| json!({}));
            let key = format!("permission-cancelled:{}", view.permission_id);
            self.inner
                .lock()
                .journal
                .push(kind::PERMISSION_RESOLVED, None, &key, payload);
        }

        let adapter = self.adapter.lock().clone();
        if let Some(adapter) = adapter {
            if let Err(error) = adapter.cancel_turn(reason) {
                // The request never reached the CLI: report it, and keep the
                // state unknown rather than pretending anything stopped.
                let mut inner = self.inner.lock();
                inner.cancel_pending = false;
                inner.interaction = InteractionStatus::Unknown;
                inner.notes.push(format!(
                    "取消请求未能送达 CLI：{}；该回合可能仍在执行",
                    error.message
                ));
                return Err(error);
            }
        }
        drop(wire);

        if had_work {
            let deadline = Instant::now() + grace;
            loop {
                {
                    let inner = self.inner.lock();
                    let still_running = inner
                        .turn
                        .as_ref()
                        .is_some_and(|turn| turn.state == "running");
                    if !still_running {
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }

        let mut inner = self.inner.lock();
        let confirmed = inner
            .turn
            .as_ref()
            .is_none_or(|turn| turn.state != "running");
        if confirmed {
            inner.cancel_pending = false;
            if let Some(turn) = inner.turn.as_ref() {
                let id = turn.turn_id.clone();
                if !inner.retired_turns.contains(&id) {
                    inner.retired_turns.push(id);
                }
            }
            inner.interaction = if inner
                .permissions
                .iter()
                .any(|slot| slot.view.state == "pending")
            {
                InteractionStatus::AwaitingPermission
            } else {
                InteractionStatus::Idle
            };
        } else {
            // Still running as far as this client knows. `cancel_pending` stays
            // set so a new message retires this turn explicitly instead of
            // silently sharing the connection with it.
            inner.interaction = InteractionStatus::Unknown;
            inner.notes.push(format!(
                "CLI 未在 {} ms 内确认取消；该回合仍可能执行，状态保持未知",
                grace.as_millis()
            ));
        }
        let view = self.view_locked(&inner);
        drop(inner);
        self.emit_state();
        Ok(view)
    }

    pub fn answer(&self, permission_id: &str, option_id: &str) -> Result<RuntimeRunView, AppError> {
        // Acquire before checking the deadline and lifecycle: time spent
        // waiting for an earlier protocol call does not extend this request.
        let _wire = self.wire.lock();
        let (native_id, kind) = {
            let mut inner = self.inner.lock();
            let unavailable = inner.closed
                || inner.execution.is_terminal()
                || inner.execution == ExecutionStatus::Stopping
                || self.stop_requested.load(Ordering::SeqCst)
                || inner.cancel_pending
                || !inner.accepting_permissions;
            let Some(slot) = inner
                .permissions
                .iter_mut()
                .find(|slot| slot.view.permission_id == permission_id)
            else {
                return Err(AppError::not_found("permission", permission_id)
                    .with_details(json!({ "runId": self.run_id })));
            };
            if slot.view.state != "pending" {
                return Err(AppError::conflict(
                    "permission",
                    format!(
                        "this request is {} and can no longer be answered",
                        slot.view.state
                    ),
                )
                .with_details(json!({
                    "runId": self.run_id,
                    "permissionId": permission_id,
                    "state": slot.view.state,
                })));
            }
            if unavailable {
                return Err(AppError::conflict(
                    "permission",
                    "the run or turn no longer accepts permission answers",
                )
                .with_details(json!({"runId":self.run_id,"permissionId":permission_id})));
            }
            if !slot.answerable || slot.in_flight {
                return Err(AppError::conflict(
                    "permission",
                    "this request is not open for an answer right now",
                )
                .with_details(json!({
                    "runId": self.run_id,
                    "permissionId": permission_id,
                    "reason": slot.view.reason,
                    "inFlight": slot.in_flight,
                })));
            }
            // The sweeper runs on a timer, so between the deadline and the next
            // tick a request still looks pending. The real deadline decides
            // here, and it also records the deny we still owe the peer: refusing
            // the late approval must not leave the CLI waiting forever.
            if slot.deadline <= Instant::now() {
                slot.view.state = "expired".into();
                slot.view.auto_denied = true;
                slot.view.reason =
                    Some("超过应答时限，已按拒绝处理；本次批准未发送，目标操作未执行".into());
                slot.view.resolved_at = Some(now_iso());
                slot.answerable = false;
                slot.obligation = deny_option_of(&slot.view.options).map(|option_id| Obligation {
                    option_id,
                    reason: "应答时限已过，按拒绝收敛对端等待".into(),
                });
                let view = slot.view.clone();
                let tool_call_id = view.tool_call_id.clone();
                inner.journal.push(
                    kind::PERMISSION_RESOLVED,
                    None,
                    &format!("permission-expired:{}", view.permission_id),
                    serde_json::to_value(&view).unwrap_or_else(|_| json!({})),
                );
                if let Some(tool_call_id) = tool_call_id.as_deref() {
                    if let Some(tool) = inner
                        .tools
                        .iter_mut()
                        .find(|tool| tool.view.tool_call_id == tool_call_id)
                    {
                        tool.view.permission_id = Some(view.permission_id.clone());
                        tool.view.authorization = "auto-denied".into();
                    }
                }
                return Err(AppError::new(
                    ErrorCode::Timeout,
                    "the permission request expired before this answer",
                )
                .with_details(json!({
                    "runId": self.run_id,
                    "permissionId": permission_id,
                    "stage": "answerDeadline",
                    "denyStillOwed": true,
                })));
            }
            let Some(option) = slot
                .view
                .options
                .iter()
                .find(|option| option.option_id == option_id)
            else {
                return Err(AppError::invalid_argument(
                    "optionId",
                    "must be one of the options this request offered",
                )
                .with_details(json!({
                    "runId": self.run_id,
                    "permissionId": permission_id,
                    "offered": slot
                        .view
                        .options
                        .iter()
                        .map(|option| option.option_id.clone())
                        .collect::<Vec<_>>(),
                })));
            };
            let kind = option.kind.clone();
            let native = slot.view.native_request_id.clone().unwrap_or_default();
            // Reserve the slot: a second answer and `cancel` both see it as busy
            // until this call settles.
            slot.in_flight = true;
            slot.answerable = false;
            slot.not_applied = None;
            (native, kind)
        };

        // The adapter is taken after validation (an invalid option must be
        // reported as such even with no connection) but the reservation is
        // rolled back if it is gone: leaving `in_flight` set would make the
        // slot permanently unanswerable, and the sweeper skips busy slots.
        let adapter = match self.adapter.lock().clone() {
            Some(adapter) => adapter,
            None => {
                let mut inner = self.inner.lock();
                if let Some(slot) = inner
                    .permissions
                    .iter_mut()
                    .find(|slot| slot.view.permission_id == permission_id)
                {
                    slot.in_flight = false;
                    slot.answerable = true;
                }
                return Err(
                    AppError::conflict("run", "the protocol connection is not attached")
                        .with_details(json!({ "runId": self.run_id })),
                );
            }
        };
        // Serialise with `cancel`. If the request was cancelled while we were
        // reserving, the re-check below refuses to write instead of committing
        // a decision on a turn the user already abandoned.
        {
            let mut inner = self.inner.lock();
            let still_open = inner.permissions.iter().any(|slot| {
                slot.view.permission_id == permission_id
                    && slot.view.state == "pending"
                    && slot.in_flight
            });
            if !still_open {
                if let Some(slot) = inner
                    .permissions
                    .iter_mut()
                    .find(|slot| slot.view.permission_id == permission_id)
                {
                    slot.in_flight = false;
                }
                return Err(AppError::conflict(
                    "permission",
                    "the request was cancelled before this answer was written",
                )
                .with_details(json!({
                    "runId": self.run_id,
                    "permissionId": permission_id,
                })));
            }
        }
        let outcome = adapter.answer_permission(&native_id, option_id, &kind);

        match outcome {
            Ok(()) => {
                let mut inner = self.inner.lock();
                let mut deferred_note: Option<String> = None;
                let mut decision_record = None;
                if let Some(slot) = inner
                    .permissions
                    .iter_mut()
                    .find(|slot| slot.view.permission_id == permission_id)
                {
                    slot.in_flight = false;
                    let superseded = slot.view.state != "pending";
                    let not_applied = slot.not_applied.clone();
                    if superseded {
                        // The request was cancelled or expired while the answer
                        // was on the wire. The decision was submitted, but it
                        // must never rewrite that terminal state.
                        slot.answerable = false;
                        deferred_note = Some(format!(
                            "权限 {permission_id} 的应答已发出，但请求在此期间已变为 {}；不覆盖该状态",
                            slot.view.state
                        ));
                    } else if let Some(state) = not_applied {
                        // Submitted, not applied: keep it answerable so the user
                        // can retry or cancel, and say so instead of pretending
                        // the decision took effect.
                        slot.answerable = true;
                        deferred_note = Some(format!(
                            "权限 {permission_id} 的决定未被对端应用（{state}）；仍待答复"
                        ));
                    } else {
                        slot.view.state = "answered".into();
                        slot.view.selected_option_id = Some(option_id.to_string());
                        slot.view.resolved_at = Some(now_iso());
                        slot.view.reason = None;
                        let view = slot.view.clone();
                        // Outbound ledger: the only dependable record of "the
                        // user decided", because two of the three products never
                        // acknowledge and CodeBuddy's ack is a pre-registration.
                        let authorization = authorization_for_view(&view);
                        if let Some(tool_call_id) = view.tool_call_id.as_deref() {
                            if let Some(tool) = inner
                                .tools
                                .iter_mut()
                                .find(|tool| tool.view.tool_call_id == tool_call_id)
                            {
                                tool.view.permission_id = Some(view.permission_id.clone());
                                tool.view.authorization = authorization;
                            }
                        }
                        inner.journal.push(
                            kind::PERMISSION_RESOLVED,
                            None,
                            &format!("permission-answered:{}", view.permission_id),
                            serde_json::to_value(&view).unwrap_or_else(|_| json!({})),
                        );
                        decision_record = Some(self.durable(
                            &inner,
                            kind::PERMISSION_RESOLVED,
                            serde_json::to_value(&view).unwrap_or_else(|_| json!({})),
                            None,
                            format!("permission-answered:{}", view.permission_id),
                        ));
                        if !inner.cancel_pending
                            && !self.stop_requested.load(Ordering::SeqCst)
                            && !inner
                                .permissions
                                .iter()
                                .any(|other| other.view.state == "pending")
                        {
                            inner.interaction = InteractionStatus::Working;
                        }
                    }
                }
                if let Some(note) = deferred_note {
                    inner.notes.push(note);
                }
                // `view()` takes the same lock: drop the guard first or this
                // deadlocks on a non-reentrant mutex.
                drop(inner);
                if let Some(record) = decision_record {
                    if let Err(error) = self.persist(&record) {
                        self.note(format!(
                            "权限决定已发送，但未能写入领域库：{}",
                            error.message
                        ));
                    }
                }
                self.emit_state();
                Ok(self.view())
            }
            Err(error) => {
                let mut inner = self.inner.lock();
                if let Some(slot) = inner
                    .permissions
                    .iter_mut()
                    .find(|slot| slot.view.permission_id == permission_id)
                {
                    slot.in_flight = false;
                    if slot.view.state != "pending" {
                        // Already superseded: nothing to restore.
                        slot.answerable = false;
                    } else if answer_failure_is_definitely_unsent(&error) {
                        // Nothing reached the peer, so the request is still
                        // open. Restoring it is what makes a retry possible.
                        slot.answerable = true;
                        slot.view.reason = Some(format!(
                            "上次应答未发送成功（未写入任何字节）：{}；可重试",
                            error.message
                        ));
                    } else {
                        // Unknown outcome: a blind retry could double-grant.
                        slot.answerable = false;
                        slot.view.reason = Some(format!(
                            "应答发送结果未知：{}；请取消本轮或结束运行，不要重复批准",
                            error.message
                        ));
                    }
                    inner
                        .notes
                        .push(format!("权限 {permission_id} 应答失败：{}", error.message));
                }
                Err(error)
            }
        }
    }

    /// Expires permission requests whose deadline passed. The protocol's own
    /// deny option is used, so the CLI is never left blocked and nothing is
    /// ever auto-allowed.
    pub fn sweep_permissions(&self) {
        let (expired_views, obligations) = {
            let mut inner = self.inner.lock();
            let now = Instant::now();
            let mut views: Vec<RuntimePermissionView> = Vec::new();
            for index in 0..inner.permissions.len() {
                let slot = &mut inner.permissions[index];
                // An answer on the wire is not expired under its feet; it will
                // settle through `answer`.
                if slot.view.state != "pending" || slot.deadline > now || slot.in_flight {
                    continue;
                }
                slot.view.state = "expired".into();
                slot.view.auto_denied = true;
                slot.view.reason = Some("超过等待时限，已按拒绝处理；目标操作未执行".into());
                slot.view.resolved_at = Some(now_iso());
                slot.answerable = false;
                slot.obligation = deny_option_of(&slot.view.options).map(|option_id| Obligation {
                    option_id,
                    reason: "应答时限已过，按拒绝收敛对端等待".into(),
                });
                views.push(slot.view.clone());
            }
            if !inner
                .permissions
                .iter()
                .any(|slot| slot.view.state == "pending")
                && inner.interaction == InteractionStatus::AwaitingPermission
            {
                inner.interaction = InteractionStatus::Working;
            }
            // Take every outstanding obligation. Taking it is what makes the
            // send exactly-once: a failure puts it back for the next tick.
            let mut taken: Vec<(String, String, String)> = Vec::new();
            for slot in inner.permissions.iter_mut() {
                if let Some(obligation) = slot.obligation.take() {
                    match slot.view.native_request_id.clone() {
                        Some(native) => taken.push((
                            native,
                            obligation.option_id,
                            slot.view.permission_id.clone(),
                        )),
                        None => slot.obligation = Some(obligation),
                    }
                }
            }
            (views, taken)
        };

        for view in &expired_views {
            let payload = serde_json::to_value(view).unwrap_or_else(|_| json!({}));
            let key = format!("permission-expired:{}", view.permission_id);
            self.inner
                .lock()
                .journal
                .push(kind::PERMISSION_RESOLVED, None, &key, payload);
            if let Some(tool_call_id) = view.tool_call_id.as_deref() {
                let mut inner = self.inner.lock();
                if let Some(tool) = inner
                    .tools
                    .iter_mut()
                    .find(|tool| tool.view.tool_call_id == tool_call_id)
                {
                    tool.view.permission_id = Some(view.permission_id.clone());
                    tool.view.authorization = "auto-denied".into();
                }
            }
        }

        if obligations.is_empty() {
            if !expired_views.is_empty() {
                self.emit_state();
            }
            return;
        }

        let adapter = self.adapter.lock().clone();
        for (native, option, permission_id) in obligations {
            let Some(adapter) = adapter.clone() else {
                // No connection: keep the duty and report it rather than
                // claiming the peer was told.
                self.restore_obligation(&permission_id, native, option, "连接不可用");
                continue;
            };
            match adapter.answer_permission(&native, &option, "deny") {
                Ok(()) => {
                    let mut inner = self.inner.lock();
                    inner
                        .notes
                        .push(format!("权限 {permission_id} 已按拒绝回送，对端等待已收敛"));
                }
                Err(error) => {
                    self.restore_obligation(&permission_id, native, option, &error.message);
                }
            }
        }
        self.emit_state();
    }

    /// Puts a deny back on the slot so the next sweep retries it.
    fn restore_obligation(
        &self,
        permission_id: &str,
        native: String,
        option: String,
        reason: &str,
    ) {
        let mut inner = self.inner.lock();
        if let Some(slot) = inner
            .permissions
            .iter_mut()
            .find(|slot| slot.view.permission_id == permission_id)
        {
            slot.obligation = Some(Obligation {
                option_id: option,
                reason: reason.to_string(),
            });
        }
        let _ = native;
        inner.notes.push(format!(
            "权限 {permission_id} 的拒绝尚未送达对端（{reason}）；下次清理时重试"
        ));
    }

    /// Ends the run and its owned tree. External processes are never touched.
    pub fn stop(&self, reason: &str, grace: Duration) -> RuntimeRunView {
        self.stop_requested.store(true, Ordering::SeqCst);
        let already_terminal = {
            let mut inner = self.inner.lock();
            invalidate_run_permissions(&mut inner, reason);
            if inner.execution.is_terminal() {
                true
            } else {
                inner.execution = ExecutionStatus::Stopping;
                inner.accepting_permissions = false;
                let interaction = inner.interaction.as_str();
                let payload = json!({
                    "executionState": ExecutionStatus::Stopping.as_str(),
                    "interactionState": interaction,
                    "reason": reason,
                });
                inner
                    .journal
                    .push(kind::RUN_STATE, None, "run/stopping", payload);
                false
            }
        };
        // Wait for an already-started answer before closing its transport.
        // New answers fail the lifecycle checks once they acquire this gate.
        let _wire = self.wire.lock();
        if !already_terminal {
            if let Some(adapter) = self.adapter.lock().clone() {
                let _ = adapter.cancel_turn(reason);
            }
            let process = self.process.lock().clone();
            if let Some(process) = process {
                if let Some(code) = process.wait_timeout(grace) {
                    self.finish(code, Some(reason.to_string()));
                } else {
                    process.kill_tree();
                    self.finish(
                        -1,
                        Some(format!(
                            "{reason}（进程未在宽限期内退出，已结束自有进程树）"
                        )),
                    );
                }
            } else {
                self.finish(-1, Some(reason.to_string()));
            }
            if let Some(adapter) = self.adapter.lock().take() {
                adapter.shutdown();
            }
        }
        // Barrier: if another thread is still committing the terminal row, this
        // blocks until it finishes. Returning earlier is what let the
        // application quit with the Run still `starting`.
        {
            let gate = self.finalize_gate.lock();
            drop(gate);
        }
        // A commit that failed earlier (for example while SQLite was briefly
        // locked) leaves `terminal_persisted` false. Promising to retry is only
        // honest if this path actually does it.
        if !self.inner.lock().terminal_persisted {
            self.persist_terminal_if_needed();
        }
        if !self.inner.lock().terminal_persisted {
            let mut inner = self.inner.lock();
            let error = AppError::new(
                ErrorCode::Io,
                "the run's terminal state could not be persisted; the database may still show an earlier status",
            )
            .with_details(json!({ "runId": self.run_id, "stage": "terminalCommit" }))
            .with_retryable(true);
            inner.error = Some(error);
            inner
                .notes
                .push("终态仍未写入领域库；内存与数据库可能不一致，需重试".to_string());
        }
        self.view()
    }

    fn finish(&self, code: i32, reason: Option<String>) {
        let gate = self.finalize_gate.lock();
        let mut inner = self.inner.lock();
        if inner.execution.is_terminal() {
            return;
        }
        inner.execution = ExecutionStatus::Exited;
        invalidate_run_permissions(&mut inner, "运行已结束，旧权限请求失效");
        inner.interaction = InteractionStatus::Unknown;
        // A run that is over cannot grant anything: a permission frame that
        // arrives after the exit is late by definition.
        inner.accepting_permissions = false;
        inner.ended_at = Some(now_iso());
        if let Some(reason) = reason {
            inner.notes.push(reason.clone());
            let payload = json!({
                "executionState": ExecutionStatus::Exited.as_str(),
                "interactionState": InteractionStatus::Unknown.as_str(),
                "exitCode": code,
                "reason": reason,
            });
            inner
                .journal
                .push(kind::RUN_STATE, None, "run/exited", payload);
        }
        inner.closed = true;
        let ended = inner.ended_at.clone().unwrap_or_else(now_iso);
        let already = inner.terminal_persisted;
        drop(inner);
        if !already {
            match self.service.finish_run(
                &self.run_id,
                ExecutionStatus::Exited,
                Some(&ended),
                Some("multicli-runtime-v1: the run ended"),
            ) {
                Ok(()) => {
                    let mut inner = self.inner.lock();
                    inner.terminal_persisted = true;
                    drop(inner);
                    self.closed.store(true, Ordering::SeqCst);
                }
                Err(error) => {
                    // Leave `terminal_persisted` false so another exit path can
                    // retry the commit instead of being permanently locked out.
                    self.inner
                        .lock()
                        .notes
                        .push(format!("终态未能写入领域库，将重试：{}", error.message));
                    eprintln!("[multicli] failed to close run {}: {error}", self.run_id);
                }
            }
        }
        drop(gate);
        self.emit_state();
    }

    pub fn is_terminal(&self) -> bool {
        self.inner.lock().execution.is_terminal()
    }

    pub fn has_pending_permission(&self) -> bool {
        self.inner
            .lock()
            .permissions
            .iter()
            .any(|slot| slot.view.state == "pending")
    }

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    pub fn view(&self) -> RuntimeRunView {
        let inner = self.inner.lock();
        self.view_locked(&inner)
    }

    fn view_locked(&self, inner: &RunInner) -> RuntimeRunView {
        let mut notes = inner.notes.clone();
        if inner.late_events > 0 {
            notes.push(format!(
                "已丢弃 {} 个来自旧连接的晚到事件",
                inner.late_events
            ));
        }
        if inner.duplicate_events > 0 {
            notes.push(format!(
                "已忽略 {} 个重复投递的事件（同一身份已处理）",
                inner.duplicate_events
            ));
        }
        for (method, count) in inner.journal.unknown_kinds() {
            notes.push(format!("保留了 {count} 条未识别的协议事件：{method}"));
        }
        RuntimeRunView {
            schema_version: RUNTIME_SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            session_id: self.session_id.clone(),
            agent_id: self.agent_id.clone(),
            installation_id: self.installation_id.clone(),
            product: self.product.product_key().to_string(),
            product_name: self.product.display_name().to_string(),
            transport: self.product.transport().to_string(),
            title: self.title.clone(),
            cwd: self.cwd.clone(),
            execution_state: inner.execution.as_str().to_string(),
            interaction_state: inner.interaction.as_str().to_string(),
            turn: inner.turn.clone(),
            native_session_id: inner.native_session_id.clone(),
            process: self.process_identity(),
            launch_argv: self.launch_argv.lock().clone(),
            messages: inner.messages.clone(),
            tools: inner.tools.iter().map(|tool| tool.view.clone()).collect(),
            permissions: inner
                .permissions
                .iter()
                .map(|slot| slot.view.clone())
                .collect(),
            last_seq: inner.journal.last_seq(),
            journal_truncated: inner.journal.is_truncated(),
            started_at: self.started_at.clone(),
            ended_at: inner.ended_at.clone(),
            resumable: inner.native_session_id.is_some(),
            notes,
            error: inner.error.clone(),
        }
    }

    pub fn read(&self, cursor: Option<u64>, limit: usize) -> super::model::RuntimeEventPage {
        let inner = self.inner.lock();
        let (items, next) = inner.journal.read(cursor, limit);
        super::model::RuntimeEventPage {
            schema_version: RUNTIME_SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            session_id: self.session_id.clone(),
            items,
            next_cursor: next,
            dropped_before: inner.journal.dropped_before(),
            last_seq: inner.journal.last_seq(),
        }
    }

    // -----------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------

    fn durable(
        &self,
        inner: &RunInner,
        kind: &str,
        payload: Value,
        native_event_id: Option<String>,
        key: String,
    ) -> PersistRecord {
        PersistRecord {
            kind: kind.to_string(),
            payload,
            native_event_id,
            key,
            seq: inner.journal.last_seq(),
        }
    }

    fn persist(&self, record: &PersistRecord) -> Result<(), AppError> {
        let message = Message {
            id: format!("{}:{}", self.run_id, record.key),
            session_id: self.session_id.clone(),
            run_id: Some(self.run_id.clone()),
            native_event_id: record.native_event_id.clone(),
            source_key: format!("runtime:{}:{}", self.run_id, record.key),
            seq: record.seq,
            kind: record.kind.clone(),
            content: if record.payload.is_object() {
                record.payload.clone()
            } else {
                json!({ "value": record.payload })
            },
            source_ref: Some(format!(
                "multicli-runtime-v1:{}",
                self.product.product_key()
            )),
            observed_at: now_iso(),
        };
        self.service.record_message(&message)?;
        Ok(())
    }

    /// Creates the frozen `Run` row this runtime owns. Called by the manager
    /// before the adapter starts so a failed spawn still leaves a real record.
    pub fn begin(&self) -> Result<(), AppError> {
        self.service.start_run(&Run {
            id: self.run_id.clone(),
            session_id: self.session_id.clone(),
            installation_id: self.installation_id.clone(),
            transport: match self.product {
                super::model::RuntimeProduct::CodeBuddy => RunTransport::Acp,
                _ => RunTransport::Native,
            },
            ownership: RunOwnership::Managed,
            started_at: Some(self.started_at.clone()),
            ended_at: None,
            execution_status: ExecutionStatus::Starting,
            interaction_status: InteractionStatus::Unknown,
            evidence_level: EvidenceLevel::Observed,
            evidence: Some(format!(
                "multicli-runtime-v1: {} {}",
                self.product.product_key(),
                self.product.transport()
            )),
        })?;
        Ok(())
    }

    pub fn fail(&self, error: &AppError) {
        let _gate = self.finalize_gate.lock();
        {
            let mut inner = self.inner.lock();
            invalidate_run_permissions(&mut inner, "运行失败，旧权限请求失效");
            inner.execution = ExecutionStatus::Failed;
            inner.error = Some(error.clone());
            inner.ended_at = Some(now_iso());
            inner.closed = true;
            // Cleared here and set only after the row is written: claiming it
            // first is what made a failed commit invisible to `stop`'s retry.
            inner.terminal_persisted = false;
            inner
                .notes
                .push(format!("{}: {}", error.code.as_str(), error.message));
            inner.journal.push(
                kind::RUN_STATE,
                None,
                "run/failed",
                json!({
                    "executionState": ExecutionStatus::Failed.as_str(),
                    "code": error.code.as_str(),
                    "message": error.message,
                }),
            );
        }
        self.closed.store(true, Ordering::SeqCst);
        let ended = now_iso();
        if let Err(failure) = self.service.finish_run(
            &self.run_id,
            ExecutionStatus::Failed,
            Some(&ended),
            Some(&format!(
                "multicli-runtime-v1: failed ({}): {}",
                error.code.as_str(),
                error.message
            )),
        ) {
            // A later `stop` retries this; saying it succeeded would hide a row
            // that still reads `starting`.
            let mut inner = self.inner.lock();
            inner.notes.push(format!(
                "终态未能写入领域库（{failure}）；stop 会重试，数据库可能仍显示先前状态"
            ));
        } else {
            self.inner.lock().terminal_persisted = true;
        }
        self.emit_state();
    }

    pub fn note(&self, text: impl Into<String>) {
        self.inner.lock().notes.push(text.into());
    }

    pub fn known_tools(&self) -> usize {
        self.inner.lock().tools.len()
    }

    pub fn known_permissions(&self) -> usize {
        self.inner.lock().permissions.len()
    }

    /// All pending requests, for the manager's sweeper.
    /// True when the sweeper has anything to do: a request that can still
    /// expire, **or** a deny still owed to the peer. The second half matters:
    /// obligations only exist on slots that already left `pending`, so gating
    /// the sweep on pending ids would leave every owed deny unsent.
    pub fn has_pending_work(&self) -> bool {
        self.inner
            .lock()
            .permissions
            .iter()
            .any(|slot| slot.view.state == "pending" || slot.obligation.is_some())
    }

    pub fn pending_permission_ids(&self) -> Vec<String> {
        self.inner
            .lock()
            .permissions
            .iter()
            .filter(|slot| slot.view.state == "pending")
            .map(|slot| slot.view.permission_id.clone())
            .collect()
    }

    /// Builds a `RuntimeToolView` map for tests and reports without exposing
    /// the internal slot type.
    pub fn tools_by_id(&self) -> BTreeMap<String, RuntimeToolView> {
        self.inner
            .lock()
            .tools
            .iter()
            .map(|tool| (tool.view.tool_call_id.clone(), tool.view.clone()))
            .collect()
    }
}

struct PersistRecord {
    kind: String,
    payload: Value,
    native_event_id: Option<String>,
    key: String,
    seq: u64,
}

/// Appends `text` at character offset `index`. Returns the part actually
/// appended (empty when the whole delta was already known) and, when the
/// offset skipped ahead, the offset the run expected.
fn append_delta(
    message: &mut RuntimeMessageView,
    index: usize,
    text: &str,
) -> (String, Option<usize>) {
    let current = message.text.chars().count();
    if index == current {
        message.text.push_str(text);
        return (text.to_string(), None);
    }
    if index < current {
        // Overlapping re-delivery: keep only what is genuinely new.
        let skip = current - index;
        if skip >= text.chars().count() {
            return (String::new(), None);
        }
        let tail: String = text.chars().skip(skip).collect();
        message.text.push_str(&tail);
        return (tail, None);
    }
    message.text.push_str(text);
    (text.to_string(), Some(current))
}

/// Helper used by `apply` when a delta arrives before the message exists.
pub(crate) fn ensure_message<'a>(
    messages: &'a mut Vec<RuntimeMessageView>,
    message_id: &str,
    seq: u64,
) -> &'a mut RuntimeMessageView {
    if let Some(position) = messages
        .iter()
        .position(|message| message.message_id == message_id)
    {
        return &mut messages[position];
    }
    messages.push(RuntimeMessageView {
        message_id: message_id.to_string(),
        role: "assistant".into(),
        text: String::new(),
        complete: false,
        seq,
    });
    messages.last_mut().expect("just pushed")
}

/// The option this client uses to refuse a request without any user input.
/// Prefers an explicit deny, then a cancel, and never an allow.
fn deny_option_of(options: &[RuntimePermissionOption]) -> Option<String> {
    options
        .iter()
        .find(|option| option.kind.starts_with("deny"))
        .or_else(|| options.iter().find(|option| option.kind == "cancel"))
        .map(|option| option.option_id.clone())
}

/// True when the adapter proved that **no byte** reached the peer. Only then may
/// the request be answered again; an unknown outcome must not be retried,
/// because a second allow could be a duplicate grant.
/// End the local grant path without inventing a denial or execution outcome.
/// No reply can be delivered after the owned connection has ended.
fn invalidate_run_permissions(inner: &mut RunInner, reason: &str) {
    inner.accepting_permissions = false;
    for slot in &mut inner.permissions {
        slot.answerable = false;
        slot.obligation = None;
        if slot.view.state == "pending" {
            slot.view.state = "cancelled".into();
            slot.view.reason = Some(reason.into());
            slot.view.resolved_at = Some(now_iso());
            let view = &slot.view;
            inner.journal.push(
                kind::PERMISSION_RESOLVED,
                None,
                &format!("permission-run-ended:{}", view.permission_id),
                serde_json::to_value(view).unwrap_or_else(|_| json!({})),
            );
        }
    }
}

fn answer_failure_is_definitely_unsent(error: &AppError) -> bool {
    error.retryable && matches!(error.code, ErrorCode::Io | ErrorCode::Protocol)
}

/// Maps a permission record onto the tool's authorization ledger.
///
/// `answered` alone is not enough: the user may have picked the deny option on
/// a request whose state is still `answered`, and "always allow" is a different
/// decision from a one-shot allow. The selected option's own kind decides.
fn authorization_for_view(view: &RuntimePermissionView) -> String {
    match view.state.as_str() {
        "pending" => "pending".into(),
        "expired" => "auto-denied".into(),
        "cancelled" => "denied".into(),
        "answered" => {
            let kind = view
                .selected_option_id
                .as_deref()
                .and_then(|id| view.options.iter().find(|option| option.option_id == id))
                .map(|option| option.kind.as_str())
                .unwrap_or("unknown");
            match kind {
                "allow-always" => "allowed-always".into(),
                "allow" => "allowed".into(),
                "deny" | "deny-always" | "cancel" => "denied".into(),
                _ => "unknown".into(),
            }
        }
        _ => "unknown".into(),
    }
}

/// Maps a terminal tool status onto the result ledger. `is_error` is only one
/// input: a call can complete and still have failed, and a denial is not a
/// failure of the tool but an absence of execution.
fn outcome_for(status: &str, is_error: bool) -> String {
    match status {
        "completed" if is_error => "failed".into(),
        "completed" => "ok".into(),
        "failed" => "failed".into(),
        "denied" | "cancelled" => "unknown".into(),
        _ => "unknown".into(),
    }
}

/// Maps a tool status onto the execution ledger.
///
/// `failed` is ambiguous: it covers "ran and exited non-zero" and "never
/// started". Only the presence of a result body distinguishes them, so without
/// one the honest answer is `unknown` — claiming `not-executed` would tell the
/// user no operation happened when one may have.
fn execution_for(status: &str, has_result_body: bool) -> String {
    match status {
        "running" | "pending" | "started" => "running".into(),
        "completed" => "executed".into(),
        "failed" => {
            if has_result_body {
                "executed".into()
            } else {
                "unknown".into()
            }
        }
        "denied" | "cancelled" => "not-executed".into(),
        _ => "unknown".into(),
    }
}

/// Display helper shared with the manager: a run is "live" while it can still
/// accept input.
pub fn is_live(execution: ExecutionStatus) -> bool {
    !execution.is_terminal()
}

/// The HTTP-free equivalent of the old engine's "terminal message" helper: a
/// short, redacted description of why a process ended.
pub fn exit_note(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("进程已退出，退出码 {code}"),
        None => "进程已退出，未取得退出码".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_append_is_idempotent_and_gap_aware() {
        let mut message = RuntimeMessageView {
            message_id: "m".into(),
            role: "assistant".into(),
            text: String::new(),
            complete: false,
            seq: 1,
        };
        let (applied, gap) = append_delta(&mut message, 0, "你好");
        assert_eq!(applied, "你好");
        assert!(gap.is_none());
        assert_eq!(message.text, "你好");

        // Re-delivery of the same delta must not duplicate text.
        let (applied, gap) = append_delta(&mut message, 0, "你好");
        assert_eq!(applied, "");
        assert!(gap.is_none());
        assert_eq!(message.text, "你好");

        // Overlapping delta keeps only the new tail.
        let (applied, _) = append_delta(&mut message, 1, "好世界");
        assert_eq!(applied, "世界");
        assert_eq!(message.text, "你好世界");

        // A skipped offset reports the expected position instead of hiding it.
        let (applied, gap) = append_delta(&mut message, 10, "!");
        assert_eq!(applied, "!");
        assert_eq!(gap, Some(4));
        assert_eq!(message.text, "你好世界!");
    }

    #[test]
    fn ensure_message_creates_then_reuses() {
        let mut messages = Vec::new();
        ensure_message(&mut messages, "a", 1).text.push('x');
        ensure_message(&mut messages, "a", 2).text.push('y');
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "xy");
    }
}
