//! S05 contract tests for the unified runtime.
//!
//! These are protocol/state-machine tests, not product acceptance: they prove
//! that a duplicated event is absorbed, that a cancelled request cannot be
//! approved, that a late event from an old connection never reaches the new
//! run, and that a tool nobody was asked about is never reported as denied.
//!
//! Real CLI evidence lives in `scratch/S05/` and is quoted in
//! `docs/execution/deliveries/S05.md`; a fixture here is never reported as live.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::multicli::domain::{ExecutionStatus, InteractionStatus};
use crate::multicli::runtime::envelope::kind;
use crate::multicli::runtime::model::{
    RuntimePermissionOption, RuntimeProduct, RuntimeStartRequest, RUNTIME_SCHEMA_VERSION,
};
use crate::multicli::runtime::run::{AdapterEvent, EventMeta, RuntimeRun};
use crate::multicli::MultiCliService;

#[path = "supervisor_tests.rs"]
mod supervisor_cases;

fn temp_root(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "multicli-runtime-{name}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn service(name: &str) -> Arc<MultiCliService> {
    let root = temp_root(name);
    let service = MultiCliService::open(&root).expect("open the isolated database");
    // `sessions.host_id` is a real foreign key: the host row has to exist
    // before any session can reference it.
    service
        .register_host(&crate::multicli::Host::local("local-windows"))
        .expect("register the local host");
    service
        .register_agent(&crate::multicli::AgentDefinition {
            id: "claude".into(),
            vendor: "anthropic".into(),
            display_name: "Claude Code".into(),
        })
        .expect("register the agent");
    Arc::new(service)
}

fn run(service: &Arc<MultiCliService>, session: &str, run_id: &str) -> Arc<RuntimeRun> {
    let root = std::env::temp_dir();
    Arc::new(RuntimeRun::new(
        Arc::clone(service),
        Arc::new(|_| {}),
        run_id.to_string(),
        session.to_string(),
        RuntimeProduct::ClaudeCode.agent_id().to_string(),
        None,
        RuntimeProduct::ClaudeCode,
        "test run".into(),
        crate::multicli::runtime::model::host_path("local-windows", &root),
        Duration::from_secs(300),
        1,
    ))
}

fn options() -> Vec<RuntimePermissionOption> {
    vec![
        RuntimePermissionOption {
            option_id: "accept".into(),
            label: "允许".into(),
            kind: "allow".into(),
        },
        RuntimePermissionOption {
            option_id: "decline".into(),
            label: "拒绝".into(),
            kind: "deny".into(),
        },
    ]
}

#[test]
fn duplicate_deltas_never_duplicate_text_and_the_final_message_wins() {
    let service = service("delta");
    let run = run(&service, "s-1", "r-1");
    let generation = run.generation();
    run.accept(
        generation,
        AdapterEvent::TextDelta {
            message_id: "m-1".into(),
            index: 0,
            text: "你好".into(),
        },
        EventMeta::key("d1"),
    );
    // The same frame again: absorbed by the journal, and the message text must
    // not grow either.
    run.accept(
        generation,
        AdapterEvent::TextDelta {
            message_id: "m-1".into(),
            index: 0,
            text: "你好".into(),
        },
        EventMeta::key("d1"),
    );
    run.accept(
        generation,
        AdapterEvent::TextDelta {
            message_id: "m-1".into(),
            index: 2,
            text: "，世界".into(),
        },
        EventMeta::key("d2"),
    );
    let view = run.view();
    assert_eq!(view.messages.len(), 1);
    assert_eq!(view.messages[0].text, "你好，世界");

    // The completed message is authoritative and replaces the accumulation.
    run.accept(
        generation,
        AdapterEvent::TextCompleted {
            message_id: "m-1".into(),
            role: "assistant".into(),
            text: "你好，世界。".into(),
        },
        EventMeta::key("d3"),
    );
    let view = run.view();
    assert_eq!(view.messages[0].text, "你好，世界。");
    assert!(view.messages[0].complete);
}

#[test]
fn a_cancelled_permission_can_never_be_approved_afterwards() {
    let service = service("permission");
    let run = run(&service, "s-1", "r-1");
    let generation = run.generation();
    run.accept(
        generation,
        AdapterEvent::TurnStarted {
            turn_id: "t-1".into(),
            model: None,
            effort: None,
        },
        EventMeta::key("t"),
    );
    run.accept(
        generation,
        AdapterEvent::PermissionRequested {
            native_request_id: "native-1".into(),
            title: "写文件".into(),
            detail: None,
            kind: "file-write".into(),
            options: options(),
            tool_call_id: Some("tool-1".into()),
            turn_id: Some("t-1".into()),
        },
        EventMeta::key("p"),
    );
    let view = run.view();
    assert_eq!(view.interaction_state, "awaiting-permission");
    assert_eq!(view.permissions.len(), 1);
    assert_eq!(view.permissions[0].state, "pending");

    // The turn ends before anyone answers: the request is cancelled, not
    // silently approved.
    run.accept(
        generation,
        AdapterEvent::TurnCompleted {
            turn_id: "t-1".into(),
            status: "cancelled".into(),
            reason: Some("用户取消".into()),
        },
        EventMeta::key("end"),
    );
    let view = run.view();
    assert_eq!(view.permissions[0].state, "cancelled");

    // Answering now must be refused, and refused *before* reaching an adapter.
    let error = run
        .answer(&view.permissions[0].permission_id, "accept")
        .expect_err("a cancelled request must not be answerable");
    assert_eq!(error.code, crate::multicli::ErrorCode::Conflict);
    assert_eq!(error.details["state"], "cancelled");
}

#[test]
fn an_unknown_option_id_is_rejected_with_the_offered_list() {
    let service = service("option");
    let run = run(&service, "s-1", "r-1");
    // A permission only exists inside a grantable turn; without one the request
    // is void by design, which is a different assertion.
    start(&run, "turn-1", "start");
    let generation = run.generation();
    run.accept(
        generation,
        AdapterEvent::PermissionRequested {
            native_request_id: "native-1".into(),
            title: "写文件".into(),
            detail: None,
            kind: "file-write".into(),
            options: options(),
            tool_call_id: None,
            turn_id: None,
        },
        EventMeta::key("p"),
    );
    let permission_id = run.view().permissions[0].permission_id.clone();
    let error = run
        .answer(&permission_id, "acceptForSession")
        .expect_err("an option the request never offered must be refused");
    assert_eq!(error.code, crate::multicli::ErrorCode::InvalidArgument);
    assert_eq!(error.details["offered"], json!(["accept", "decline"]),);
    // The request is still pending: a rejected answer must not consume it.
    assert_eq!(run.view().permissions[0].state, "pending");
}

#[test]
fn events_from_a_previous_connection_never_reach_the_new_one() {
    let service = service("generation");
    let run = run(&service, "s-1", "r-1");
    let stale = run.generation() + 100;
    run.accept(
        stale,
        AdapterEvent::TextCompleted {
            message_id: "m-1".into(),
            role: "assistant".into(),
            text: "迟到内容".into(),
        },
        EventMeta::key("late"),
    );
    let view = run.view();
    assert!(view.messages.is_empty(), "a late event must be dropped");
    assert!(
        view.notes.iter().any(|note| note.contains("旧连接")),
        "the drop must be visible: {:?}",
        view.notes
    );
    assert_eq!(view.last_seq, 1, "only the starting event exists");
}

#[test]
fn a_tool_nobody_was_asked_about_is_not_a_denial() {
    let service = service("quadrant");
    let run = run(&service, "s-1", "r-1");
    let generation = run.generation();
    run.accept(
        generation,
        AdapterEvent::ToolCall {
            tool_call_id: "tool-read".into(),
            name: "Read".into(),
            kind: "search".into(),
            input: None,
            permission_id: None,
        },
        EventMeta::key("call"),
    );
    run.accept(
        generation,
        AdapterEvent::ToolUpdate {
            tool_call_id: "tool-read".into(),
            status: "completed".into(),
            output: Some("文件内容".into()),
            is_error: false,
        },
        EventMeta::key("result"),
    );
    let view = run.view();
    let tool = &view.tools[0];
    assert_eq!(tool.authorization, "not-requested");
    assert_eq!(tool.execution, "executed");
    assert_eq!(tool.outcome, "ok");
    assert_eq!(tool.quadrant(), "never-asked-executed-ok");
}

#[test]
fn a_denied_tool_is_linked_by_tool_id_and_never_executes() {
    let service = service("denied");
    let run = run(&service, "s-1", "r-1");
    let generation = run.generation();
    // Permission arrives before the tool call, which is Claude's order.
    run.accept(
        generation,
        AdapterEvent::PermissionRequested {
            native_request_id: "native-1".into(),
            title: "写文件".into(),
            detail: None,
            kind: "file-write".into(),
            options: options(),
            tool_call_id: Some("tool-write".into()),
            turn_id: None,
        },
        EventMeta::key("p"),
    );
    run.accept(
        generation,
        AdapterEvent::ToolCall {
            tool_call_id: "tool-write".into(),
            name: "Write".into(),
            kind: "file-write".into(),
            input: Some(json!({"file_path": "C:\\tmp\\a.txt"})),
            permission_id: None,
        },
        EventMeta::key("call"),
    );
    // The CLI reports that the write was refused.
    run.accept(
        generation,
        AdapterEvent::ToolUpdate {
            tool_call_id: "tool-write".into(),
            status: "denied".into(),
            output: Some("denied by user".into()),
            is_error: true,
        },
        EventMeta::key("result"),
    );
    let view = run.view();
    let tool = view
        .tools
        .iter()
        .find(|t| t.tool_call_id == "tool-write")
        .unwrap();
    assert!(
        tool.permission_id.is_some(),
        "the tool must keep its request id"
    );
    assert_eq!(tool.execution, "not-executed");
    assert_eq!(tool.outcome, "unknown");
    // Linked both ways: the permission knows which call it was about.
    assert_eq!(
        view.permissions[0].tool_call_id.as_deref(),
        Some("tool-write")
    );
}

#[test]
fn a_tool_that_never_reports_is_closed_as_unknown_when_the_turn_ends() {
    let service = service("dangling");
    let run = run(&service, "s-1", "r-1");
    let generation = run.generation();
    run.accept(
        generation,
        AdapterEvent::TurnStarted {
            turn_id: "t-1".into(),
            model: None,
            effort: None,
        },
        EventMeta::key("t"),
    );
    run.accept(
        generation,
        AdapterEvent::ToolCall {
            tool_call_id: "tool-1".into(),
            name: "Bash".into(),
            kind: "command".into(),
            input: None,
            permission_id: None,
        },
        EventMeta::key("call"),
    );
    run.accept(
        generation,
        AdapterEvent::TurnCompleted {
            turn_id: "t-1".into(),
            status: "failed".into(),
            reason: Some("网络错误".into()),
        },
        EventMeta::key("end"),
    );
    let view = run.view();
    let tool = &view.tools[0];
    assert_eq!(tool.execution, "unknown");
    assert_eq!(tool.status, "unknown");
    assert!(view.notes.iter().any(|note| note.contains("没有结果")));
}

#[test]
fn unknown_protocol_events_are_preserved_and_counted_once() {
    let service = service("unknown");
    let run = run(&service, "s-1", "r-1");
    let generation = run.generation();
    for _ in 0..3 {
        run.accept(
            generation,
            AdapterEvent::Unknown {
                method: "acme/thing".into(),
                sample: json!({"a": 1}),
            },
            EventMeta::key("acme/thing"),
        );
    }
    let view = run.view();
    assert!(
        view.notes.iter().any(|note| note.contains("acme/thing")),
        "the report must name what arrived: {:?}",
        view.notes
    );
    let page = run.read(None, 100);
    let unknown = page
        .items
        .iter()
        .filter(|event| event.kind == kind::UNKNOWN)
        .count();
    assert_eq!(unknown, 1, "repeats of the same method are deduped");
}

#[test]
fn reading_events_is_cursor_based_and_never_repeats() {
    let service = service("cursor");
    let run = run(&service, "s-1", "r-1");
    let generation = run.generation();
    for index in 0..5 {
        run.accept(
            generation,
            AdapterEvent::TextDelta {
                message_id: "m-1".into(),
                index,
                text: "x".into(),
            },
            EventMeta::key(format!("d{index}")),
        );
    }
    let first = run.read(None, 2);
    assert_eq!(first.items.len(), 2);
    let second = run.read(Some(first.next_cursor), 2);
    assert_eq!(second.items.len(), 2);
    assert!(second.items[0].seq > first.items[1].seq);
    let third = run.read(Some(second.next_cursor), 10);
    // Six events in total: the `run/starting` event plus five deltas.
    assert_eq!(third.items.len(), 2);
    assert_eq!(third.dropped_before, 0);
    assert_eq!(third.last_seq, 6);
    // The page carries the frozen envelope fields.
    let envelope = &first.items[0];
    assert_eq!(envelope.schema_version, 1);
    assert_eq!(envelope.run_id.as_deref(), Some("r-1"));
    assert_eq!(envelope.session_id.as_deref(), Some("s-1"));
    assert_eq!(envelope.stream_id, "runtime/r-1");
}

#[test]
fn durable_messages_reach_the_domain_store_and_stay_idempotent() {
    let service = service("persist");
    // A Run row has to exist before a message can reference it.
    service
        .ensure_session(&crate::multicli::Session {
            id: "s-1".into(),
            agent_id: "claude".into(),
            host_id: "local-windows".into(),
            source_namespace: "claude:runtime:local-windows".into(),
            native_id: None,
            origin: crate::multicli::SessionOrigin::Managed,
            title: "t".into(),
        })
        .unwrap();
    let run = run(&service, "s-1", "r-1");
    run.begin().expect("the run row is created");
    let generation = run.generation();
    run.accept(
        generation,
        AdapterEvent::TextCompleted {
            message_id: "m-1".into(),
            role: "assistant".into(),
            text: "持久化正文".into(),
        },
        EventMeta::key("m"),
    );
    // Re-delivered: the journal absorbs it, so nothing is written twice.
    run.accept(
        generation,
        AdapterEvent::TextCompleted {
            message_id: "m-1".into(),
            role: "assistant".into(),
            text: "持久化正文".into(),
        },
        EventMeta::key("m"),
    );
    let detail = service.get_session("s-1").unwrap();
    assert_eq!(detail.message_count, 1);
    assert_eq!(detail.runs.len(), 1);
    assert_eq!(detail.messages[0].kind, kind::MESSAGE_COMPLETED);
    assert_eq!(detail.messages[0].content["text"], "持久化正文");
    assert_eq!(detail.messages[0].run_id.as_deref(), Some("r-1"));
}

#[test]
fn run_states_are_terminal_and_never_reopen() {
    let service = service("terminal");
    service
        .ensure_session(&crate::multicli::Session {
            id: "s-1".into(),
            agent_id: "claude".into(),
            host_id: "local-windows".into(),
            source_namespace: "claude:runtime:local-windows".into(),
            native_id: None,
            origin: crate::multicli::SessionOrigin::Managed,
            title: "t".into(),
        })
        .unwrap();
    let run = run(&service, "s-1", "r-1");
    run.begin().unwrap();
    let generation = run.generation();
    run.accept(
        generation,
        AdapterEvent::Exited {
            code: Some(0),
            reason: None,
        },
        EventMeta::key("exit"),
    );
    assert_eq!(run.view().execution_state, "exited");
    assert!(run.is_terminal());
    // A later event cannot resurrect the run.
    run.accept(
        generation,
        AdapterEvent::TurnStarted {
            turn_id: "t-1".into(),
            model: None,
            effort: None,
        },
        EventMeta::key("t"),
    );
    assert_eq!(run.view().execution_state, "exited");
}

#[test]
fn start_requests_are_validated_before_anything_is_created() {
    let mut request = RuntimeStartRequest {
        installation_id: "inst-1".into(),
        cwd: std::env::temp_dir().display().to_string(),
        prompt: Some("你好 😀".into()),
        title: None,
        permission_timeout_ms: None,
        model: None,
        effort: None,
    };
    assert!(request.validate().is_ok());
    request.cwd = "relative".into();
    assert!(request.validate().is_err());
    request.cwd = std::env::temp_dir().display().to_string();
    request.prompt = Some("x".repeat(crate::multicli::runtime::model::MAX_PROMPT_BYTES + 1));
    assert!(request.validate().is_err());
}

#[test]
fn the_view_reports_the_contract_version_and_no_inferred_state() {
    let service = service("view");
    let run = run(&service, "s-1", "r-1");
    let view = run.view();
    assert_eq!(view.schema_version, RUNTIME_SCHEMA_VERSION);
    assert_eq!(view.execution_state, ExecutionStatus::Starting.as_str());
    // Nothing has spoken yet: the interaction state must stay unknown rather
    // than guess "idle" from the absence of output.
    assert_eq!(view.interaction_state, InteractionStatus::Unknown.as_str());
    assert!(!view.resumable, "no native id yet");
    assert!(view.messages.is_empty());
    assert_eq!(view.transport, "native");
    assert_eq!(view.product, "claude-code");
}

#[test]
fn a_process_exit_is_written_to_the_run_row_not_only_the_view() {
    let service = service("terminal-persist");
    service
        .ensure_session(&crate::multicli::Session {
            id: "s-1".into(),
            agent_id: "claude".into(),
            host_id: "local-windows".into(),
            source_namespace: "claude:runtime:local-windows".into(),
            native_id: None,
            origin: crate::multicli::SessionOrigin::Managed,
            title: "t".into(),
        })
        .unwrap();
    let run = run(&service, "s-1", "r-1");
    run.begin().unwrap();
    let generation = run.generation();
    run.accept(
        generation,
        AdapterEvent::Exited {
            code: Some(0),
            reason: None,
        },
        EventMeta::key("exit"),
    );
    assert_eq!(run.view().execution_state, "exited");
    // The durable row must agree with the view, otherwise a restart cannot
    // reconcile the run and a later `resume` sees a phantom active run.
    let detail = service.get_session("s-1").unwrap();
    assert_eq!(
        detail.runs[0].execution_status.as_str(),
        "exited",
        "the Run row still says {:?}",
        detail.runs[0].execution_status
    );
    assert!(detail.runs[0].ended_at.is_some());
}

// ---------------------------------------------------------------------------
// R1 regressions: the ten counterexamples the supervisor reproduced, kept here
// so the same behaviours stay covered without the private probe project.
// ---------------------------------------------------------------------------

use crate::multicli::runtime::run::Adapter;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Counts what actually reached the "wire" and never confirms a cancel.
#[derive(Default)]
struct CountingAdapter {
    answers: AtomicUsize,
    cancels: AtomicUsize,
    sends: AtomicUsize,
}

impl Adapter for CountingAdapter {
    fn native_session_id(&self) -> Option<String> {
        Some("synthetic-native".into())
    }
    fn send_prompt(&self, _text: &str) -> Result<(), crate::multicli::AppError> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn cancel_turn(&self, _reason: &str) -> Result<(), crate::multicli::AppError> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn answer_permission(
        &self,
        _id: &str,
        _option: &str,
        _kind: &str,
    ) -> Result<(), crate::multicli::AppError> {
        self.answers.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn shutdown(&self) {}
}

fn run_with_adapter_timeout(
    service: &Arc<MultiCliService>,
    session: &str,
    run_id: &str,
    timeout: Duration,
) -> (Arc<RuntimeRun>, Arc<CountingAdapter>) {
    let root = std::env::temp_dir();
    let run = Arc::new(RuntimeRun::new(
        Arc::clone(service),
        Arc::new(|_| {}),
        run_id.to_string(),
        session.to_string(),
        RuntimeProduct::ClaudeCode.agent_id().to_string(),
        None,
        RuntimeProduct::ClaudeCode,
        "test run".into(),
        crate::multicli::runtime::model::host_path("local-windows", &root),
        timeout,
        1,
    ));
    let adapter = Arc::new(CountingAdapter::default());
    run.attach_adapter(
        adapter.clone(),
        Arc::new(crate::multicli::runtime::process::OwnedProcess::detached_for_tests()),
    );
    (run, adapter)
}

fn run_with_adapter(
    service: &Arc<MultiCliService>,
    session: &str,
    run_id: &str,
) -> (Arc<RuntimeRun>, Arc<CountingAdapter>) {
    let run = run(service, session, run_id);
    let adapter = Arc::new(CountingAdapter::default());
    run.attach_adapter(
        adapter.clone(),
        Arc::new(crate::multicli::runtime::process::OwnedProcess::detached_for_tests()),
    );
    (run, adapter)
}

fn start(run: &RuntimeRun, id: &str, key: &str) {
    run.accept(
        run.generation(),
        AdapterEvent::TurnStarted {
            turn_id: id.into(),
            model: None,
            effort: None,
        },
        EventMeta::key(key),
    );
}

fn request_permission(run: &RuntimeRun) -> String {
    run.accept(
        run.generation(),
        AdapterEvent::PermissionRequested {
            native_request_id: "native-1".into(),
            title: "synthetic write".into(),
            detail: None,
            kind: "file-write".into(),
            options: options(),
            tool_call_id: Some("tool-1".into()),
            turn_id: Some("old".into()),
        },
        EventMeta::key("perm"),
    );
    run.view().permissions[0].permission_id.clone()
}

#[test]
fn cancel_without_protocol_ack_never_claims_idle() {
    let service = service("r1-cancel");
    let (run, adapter) = run_with_adapter(&service, "s-1", "r-1");
    start(&run, "old", "start");
    let view = run
        .cancel("test", Duration::from_millis(1))
        .expect("cancel");
    // The peer said nothing; claiming `idle` would tell the user the turn is
    // over while it may still be running.
    assert_eq!(view.interaction_state, "unknown");
    assert_eq!(adapter.cancels.load(Ordering::SeqCst), 1);
    // The turn is still the running one, and it is *not* reported as cancelled.
    assert_eq!(
        view.turn.as_ref().map(|t| t.state.as_str()),
        Some("running")
    );
    assert!(view.notes.iter().any(|note| note.contains("未在")));
}

#[test]
fn an_old_turn_completion_cannot_close_the_new_turn() {
    let service = service("r1-old-end");
    let (run, _adapter) = run_with_adapter(&service, "s-1", "r-1");
    start(&run, "old", "start");
    run.cancel("test", Duration::from_millis(1)).unwrap();
    let new = run.send("second prompt").unwrap().turn.unwrap();
    run.accept(
        run.generation(),
        AdapterEvent::TurnCompleted {
            turn_id: "old".into(),
            status: "cancelled".into(),
            reason: None,
        },
        EventMeta::key("late-old"),
    );
    let current = run.view().turn.unwrap();
    assert_eq!(current.turn_id, new.turn_id);
    assert_eq!(current.state, "running");
}

#[test]
fn a_replayed_start_cannot_reopen_a_completed_turn() {
    let service = service("r1-replay");
    let (run, _adapter) = run_with_adapter(&service, "s-1", "r-1");
    start(&run, "old", "same-start");
    run.accept(
        run.generation(),
        AdapterEvent::TurnCompleted {
            turn_id: "old".into(),
            status: "completed".into(),
            reason: None,
        },
        EventMeta::key("end"),
    );
    assert_eq!(run.view().turn.unwrap().state, "completed");
    // Same identity, replayed: dedupe must run before the state change.
    start(&run, "old", "same-start");
    assert_eq!(run.view().turn.unwrap().state, "completed");
}

#[test]
fn an_expired_request_is_refused_before_it_reaches_the_wire() {
    let service = service("r1-expired");
    let (run, adapter) =
        run_with_adapter_timeout(&service, "s-1", "r-1", Duration::from_millis(20));
    start(&run, "old", "start");
    let id = request_permission(&run);
    // Past the deadline, before any sweeper tick.
    std::thread::sleep(Duration::from_millis(40));
    let error = run
        .answer(&id, "accept")
        .expect_err("an expired request must not be answerable");
    assert_eq!(error.code, crate::multicli::ErrorCode::Timeout);
    assert_eq!(adapter.answers.load(Ordering::SeqCst), 0);
    assert_eq!(run.view().permissions[0].state, "expired");
}

#[test]
fn a_failed_tool_with_a_result_is_not_reported_as_never_executed() {
    let service = service("r1-failed-tool");
    let run = run(&service, "s-1", "r-1");
    run.accept(
        run.generation(),
        AdapterEvent::ToolCall {
            tool_call_id: "tool".into(),
            name: "Bash".into(),
            kind: "command".into(),
            input: None,
            permission_id: None,
        },
        EventMeta::key("call"),
    );
    run.accept(
        run.generation(),
        AdapterEvent::ToolUpdate {
            tool_call_id: "tool".into(),
            status: "failed".into(),
            output: Some("process executed and exited 1".into()),
            is_error: true,
        },
        EventMeta::key("update"),
    );
    let tool = &run.view().tools[0];
    assert_eq!(tool.execution, "executed");
    assert_eq!(tool.outcome, "failed");

    // Without any result body the same status stays `unknown`: a failure alone
    // does not prove the tool ran.
    run.accept(
        run.generation(),
        AdapterEvent::ToolCall {
            tool_call_id: "tool-2".into(),
            name: "Bash".into(),
            kind: "command".into(),
            input: None,
            permission_id: None,
        },
        EventMeta::key("call-2"),
    );
    run.accept(
        run.generation(),
        AdapterEvent::ToolUpdate {
            tool_call_id: "tool-2".into(),
            status: "failed".into(),
            output: None,
            is_error: true,
        },
        EventMeta::key("update-2"),
    );
    let tool = run
        .view()
        .tools
        .into_iter()
        .find(|tool| tool.tool_call_id == "tool-2")
        .unwrap();
    assert_eq!(tool.execution, "unknown");
}

#[test]
fn stop_waits_for_an_in_flight_terminal_commit() {
    let service = service("r1-finalize");
    service
        .ensure_session(&crate::multicli::Session {
            id: "s-1".into(),
            agent_id: "claude".into(),
            host_id: "local-windows".into(),
            source_namespace: "claude:runtime:local-windows".into(),
            native_id: None,
            origin: crate::multicli::SessionOrigin::Managed,
            title: "t".into(),
        })
        .unwrap();
    let run = run(&service, "s-1", "r-1");
    run.begin().unwrap();

    // Hold the write lock so the finalizer blocks mid-commit.
    let blocker =
        rusqlite::Connection::open(service.store().database_path()).expect("second connection");
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let finalizing = Arc::clone(&run);
    let worker = std::thread::spawn(move || {
        finalizing.accept(
            finalizing.generation(),
            AdapterEvent::Exited {
                code: Some(0),
                reason: None,
            },
            EventMeta::key("exit"),
        );
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !run.is_terminal() {
        assert!(std::time::Instant::now() < deadline, "run never finalized");
        std::thread::sleep(Duration::from_millis(1));
    }

    let (sender, receiver) = std::sync::mpsc::channel();
    let stopping = Arc::clone(&run);
    let stopper = std::thread::spawn(move || {
        let view = stopping.stop("test shutdown", Duration::ZERO);
        sender.send(view).unwrap();
    });
    let returned_early = receiver.recv_timeout(Duration::from_millis(75)).is_ok();
    let stored: String = blocker
        .query_row(
            "SELECT execution_status FROM runs WHERE id='r-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    blocker.execute_batch("ROLLBACK").unwrap();
    worker.join().unwrap();
    stopper.join().unwrap();
    assert!(
        !returned_early,
        "stop returned while the terminal row was still uncommitted (stored={stored})"
    );
}

#[test]
fn an_unknown_version_reports_unverified_rather_than_live() {
    let service = service("r1-version");
    let exe = std::path::PathBuf::from(std::env::var("SystemRoot").expect("SystemRoot"))
        .join("System32/cmd.exe");
    service
        .register_installation(&crate::multicli::AgentInstallation {
            id: "unknown-version".into(),
            host_id: "local-windows".into(),
            agent_id: "claude".into(),
            executable: crate::multicli::runtime::model::host_path("local-windows", &exe),
            version: Some("9999.0-unverified".into()),
            config_profile_id: "synthetic".into(),
        })
        .unwrap();
    let manager = crate::multicli::runtime::RuntimeManager::new(
        Arc::clone(&service),
        std::env::temp_dir(),
        "test".into(),
        Arc::new(|_| {}),
    )
    .unwrap();
    let capabilities = manager.capabilities().unwrap();
    let item = capabilities
        .items
        .iter()
        .find(|item| item.installation_id.as_deref() == Some("unknown-version"))
        .expect("the registered installation appears");
    // Capability state and evidence tier are different fields; an unmeasured
    // version inherits neither.
    assert_eq!(item.permission_flow, "unverified");
    assert_eq!(item.structured_chat, "unverified");
    assert_eq!(item.evidence, "document");
}

// ---------------------------------------------------------------------------
// R2 regressions: permission lifecycle and terminal persistence.
// ---------------------------------------------------------------------------

/// Blocks inside `answer_permission` until released, so a concurrent cancel can
/// be observed.
struct BlockingAdapter {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    grants: AtomicUsize,
}

impl Adapter for BlockingAdapter {
    fn native_session_id(&self) -> Option<String> {
        None
    }
    fn send_prompt(&self, _text: &str) -> Result<(), crate::multicli::AppError> {
        Ok(())
    }
    fn cancel_turn(&self, _reason: &str) -> Result<(), crate::multicli::AppError> {
        Ok(())
    }
    fn answer_permission(
        &self,
        _id: &str,
        _option: &str,
        _kind: &str,
    ) -> Result<(), crate::multicli::AppError> {
        let _ = self.entered.send(());
        let _ = self
            .release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5));
        self.grants.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn shutdown(&self) {}
}

/// Fails the first answer as "definitely unsent", then succeeds.
struct FailOnceAdapter {
    attempts: AtomicUsize,
}

impl Adapter for FailOnceAdapter {
    fn native_session_id(&self) -> Option<String> {
        None
    }
    fn send_prompt(&self, _text: &str) -> Result<(), crate::multicli::AppError> {
        Ok(())
    }
    fn cancel_turn(&self, _reason: &str) -> Result<(), crate::multicli::AppError> {
        Ok(())
    }
    fn answer_permission(
        &self,
        _id: &str,
        _option: &str,
        _kind: &str,
    ) -> Result<(), crate::multicli::AppError> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(crate::multicli::AppError::new(
                crate::multicli::ErrorCode::Io,
                "synthetic no-bytes-written failure",
            )
            .with_retryable(true))
        } else {
            Ok(())
        }
    }
    fn shutdown(&self) {}
}

#[test]
fn a_permission_arriving_after_cancel_cannot_be_granted() {
    let service = service("r2-late-permission");
    let (run, adapter) = run_with_adapter(&service, "s-1", "r-1");
    start(&run, "old", "start");
    // Peer never confirms the cancel.
    run.cancel("synthetic cancel", Duration::from_millis(1))
        .unwrap();
    let id = request_permission(&run);
    let error = run
        .answer(&id, "accept")
        .expect_err("a request for a cancelling turn must not open a grant path");
    assert_eq!(error.code, crate::multicli::ErrorCode::Conflict);
    assert_eq!(adapter.answers.load(Ordering::SeqCst), 0);
    // The request is void, and a deny is still owed to the peer.
    assert_eq!(run.view().permissions[0].state, "cancelled");
}

#[test]
fn an_expired_answer_still_denies_the_peer_once() {
    let service = service("r2-expired-deny");
    let (run, adapter) =
        run_with_adapter_timeout(&service, "s-1", "r-1", Duration::from_millis(20));
    start(&run, "old", "start");
    let id = request_permission(&run);
    std::thread::sleep(Duration::from_millis(40));
    assert!(run.answer(&id, "accept").is_err());
    // Refusing the late approval must not drop the obligation to answer.
    assert_eq!(adapter.answers.load(Ordering::SeqCst), 0);
    run.sweep_permissions();
    assert_eq!(adapter.answers.load(Ordering::SeqCst), 1);
    assert!(run.view().permissions[0].auto_denied);
    // A second sweep must not send it again.
    run.sweep_permissions();
    assert_eq!(adapter.answers.load(Ordering::SeqCst), 1);
}

#[test]
fn a_definitely_unsent_answer_stays_answerable() {
    let service = service("r2-answer-retry");
    let run = run(&service, "s-1", "r-1");
    let adapter = Arc::new(FailOnceAdapter {
        attempts: AtomicUsize::new(0),
    });
    run.attach_adapter(
        adapter.clone(),
        Arc::new(crate::multicli::runtime::process::OwnedProcess::detached_for_tests()),
    );
    start(&run, "old", "start");
    let id = request_permission(&run);
    assert!(run.answer(&id, "accept").is_err());
    // Nothing reached the peer, so the request must still be open.
    let retry = run.answer(&id, "decline");
    assert!(retry.is_ok(), "a known unsent failure must stay answerable");
    assert_eq!(adapter.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(run.view().permissions[0].state, "answered");
}

#[test]
fn cancel_waits_for_an_in_flight_answer_before_returning() {
    let service = service("r2-cancel-barrier");
    let run = run(&service, "s-1", "r-1");
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let adapter = Arc::new(BlockingAdapter {
        entered: entered_tx,
        release: std::sync::Mutex::new(release_rx),
        grants: AtomicUsize::new(0),
    });
    run.attach_adapter(
        adapter.clone(),
        Arc::new(crate::multicli::runtime::process::OwnedProcess::detached_for_tests()),
    );
    start(&run, "old", "start");
    let id = request_permission(&run);

    let answering = Arc::clone(&run);
    let answer_worker = std::thread::spawn(move || answering.answer(&id, "accept"));
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let (cancel_tx, cancel_rx) = std::sync::mpsc::channel();
    let cancelling = Arc::clone(&run);
    let cancel_worker = std::thread::spawn(move || {
        let result = cancelling.cancel("cancel while answer blocked", Duration::ZERO);
        cancel_tx.send(result).unwrap();
    });
    let returned_early = cancel_rx.recv_timeout(Duration::from_millis(75)).is_ok();
    release_tx.send(()).unwrap();
    let _ = answer_worker.join().unwrap();
    let _ = cancel_worker.join().unwrap();
    // Either the grant commits before cancel returns, or it never commits.
    assert!(
        !returned_early || adapter.grants.load(Ordering::SeqCst) == 0,
        "cancel returned before the in-flight grant settled"
    );
}

#[test]
fn stop_retries_a_failed_terminal_commit() {
    let service = service("r2-stop-retry");
    service
        .ensure_session(&crate::multicli::Session {
            id: "s-1".into(),
            agent_id: "claude".into(),
            host_id: "local-windows".into(),
            source_namespace: "claude:runtime:local-windows".into(),
            native_id: None,
            origin: crate::multicli::SessionOrigin::Managed,
            title: "t".into(),
        })
        .unwrap();
    let run = run(&service, "s-1", "r-1");
    run.begin().unwrap();
    let observer = rusqlite::Connection::open(service.store().database_path()).unwrap();
    observer
        .execute_batch(
            "CREATE TRIGGER r2_fail_terminal BEFORE UPDATE OF execution_status ON runs \
             WHEN NEW.execution_status IN ('exited','failed') \
             BEGIN SELECT RAISE(ABORT,'synthetic temporary durable failure'); END;",
        )
        .unwrap();
    run.accept(
        run.generation(),
        AdapterEvent::Exited {
            code: Some(0),
            reason: None,
        },
        EventMeta::key("exit"),
    );
    observer
        .execute_batch("DROP TRIGGER r2_fail_terminal")
        .unwrap();
    // The first commit failed; stop must retry instead of just returning.
    run.stop("retry after storage recovers", Duration::ZERO);
    let stored: String = observer
        .query_row(
            "SELECT execution_status FROM runs WHERE id='r-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, "exited");
}

#[test]
fn a_denial_comes_from_the_ledger_not_from_tool_text() {
    let service = service("r2-marker");
    let run = run(&service, "s-1", "r-1");
    // A successful read whose document happens to contain the old marker.
    run.accept(
        run.generation(),
        AdapterEvent::ToolCall {
            tool_call_id: "tool".into(),
            name: "Read".into(),
            kind: "search".into(),
            input: None,
            permission_id: None,
        },
        EventMeta::key("call"),
    );
    run.accept(
        run.generation(),
        AdapterEvent::ToolUpdate {
            tool_call_id: "tool".into(),
            status: "completed".into(),
            output: Some("Document read. Literal example: [MCLI:denied]".into()),
            is_error: false,
        },
        EventMeta::key("result"),
    );
    let tool = &run.view().tools[0];
    assert_eq!(tool.authorization, "not-requested");
    assert_eq!(tool.execution, "executed");
    assert_eq!(tool.status, "completed");
    // And no contract error was inferred from the text.
    assert!(run.view().error.is_none());
}

#[test]
fn a_tool_denied_in_the_ledger_is_reported_as_denied_even_if_the_result_looks_ok() {
    let service = service("r2-ledger-denied");
    let (run, _adapter) = run_with_adapter(&service, "s-1", "r-1");
    start(&run, "old", "start");
    let id = request_permission(&run);
    run.answer(&id, "decline").unwrap();
    // A result arrives that claims success; the ledger still says denied.
    run.accept(
        run.generation(),
        AdapterEvent::ToolUpdate {
            tool_call_id: "tool-1".into(),
            status: "completed".into(),
            output: Some("nothing happened".into()),
            is_error: false,
        },
        EventMeta::key("result"),
    );
    let tool = run
        .view()
        .tools
        .into_iter()
        .find(|tool| tool.tool_call_id == "tool-1")
        .expect("the tool is created from the result");
    assert_eq!(tool.authorization, "denied");
    assert_eq!(tool.status, "denied");
    assert_eq!(tool.execution, "not-executed");
}

#[test]
fn a_decision_that_was_not_applied_keeps_the_request_answerable() {
    let service = service("r2-not-applied");
    let (run, _adapter) = run_with_adapter(&service, "s-1", "r-1");
    start(&run, "old", "start");
    let id = request_permission(&run);
    let native = run.view().permissions[0]
        .native_request_id
        .clone()
        .expect("the synthetic request carries a native id");
    // The adapter reports "submitted, not applied" while the answer is on the
    // wire; the run must not close the answer path.
    run.accept(
        run.generation(),
        AdapterEvent::PermissionNotApplied {
            native_request_id: native,
            state: "pending".into(),
            detail: Some("synthetic queued".into()),
        },
        EventMeta::key("not-applied"),
    );
    assert_eq!(run.view().permissions[0].state, "pending");
    // Still answerable with another option.
    let second = run.answer(&id, "decline");
    assert!(second.is_ok());
}

// ---------------------------------------------------------------------------
// R2 follow-up: the gaps an adversarial review found in the first R2 patch.
// ---------------------------------------------------------------------------

/// The manager used to call the sweeper only when a request was still
/// `pending`, but a deny is owed precisely when the request is no longer
/// pending. This asserts the gate's own predicate, so the test cannot pass by
/// calling `sweep_permissions` directly.
#[test]
fn an_owed_deny_keeps_the_sweeper_working_after_the_request_expires() {
    let service = service("r2-sweep-owed");
    let (run, adapter) =
        run_with_adapter_timeout(&service, "s-1", "r-1", Duration::from_millis(20));
    start(&run, "old", "start");
    let id = request_permission(&run);
    std::thread::sleep(Duration::from_millis(40));
    assert!(
        run.answer(&id, "allow").is_err(),
        "a late approval must be refused"
    );
    assert!(
        run.has_pending_work(),
        "the owed deny must keep the manager's sweeper running, even though nothing is pending"
    );
    run.sweep_permissions();
    assert_eq!(
        adapter.answers.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the peer must receive exactly one deny"
    );
    assert!(
        !run.has_pending_work(),
        "the obligation was discharged once"
    );
}

/// Protocols without a turn id (Claude, CodeBuddy) have no other ownership
/// signal. After a *confirmed* cancel, `cancel_pending` is cleared, so the flag
/// has to do the work: a late request must not become answerable again.
#[test]
fn a_request_without_a_turn_id_is_void_after_a_confirmed_cancel() {
    let service = service("r2-no-turn-id");
    let (run, adapter) = run_with_adapter(&service, "s-1", "r-1");
    start(&run, "old", "start");
    run.cancel("user cancelled", Duration::ZERO)
        .expect("a running turn can be cancelled");
    run.accept(
        run.generation(),
        AdapterEvent::TurnCompleted {
            turn_id: "old".into(),
            status: "cancelled".into(),
            reason: None,
        },
        EventMeta::key("confirmed"),
    );
    // Same protocol surface as Claude: no turn id at all.
    run.accept(
        run.generation(),
        AdapterEvent::PermissionRequested {
            native_request_id: "native-late".into(),
            title: "late write".into(),
            detail: None,
            kind: "file-write".into(),
            options: options(),
            tool_call_id: Some("tool-late".into()),
            turn_id: None,
        },
        EventMeta::key("late"),
    );
    let view = run
        .view()
        .permissions
        .into_iter()
        .find(|permission| permission.native_request_id.as_deref() == Some("native-late"))
        .expect("the late request is recorded");
    let id = view.permission_id.clone();
    assert_eq!(
        view.state, "cancelled",
        "a request arriving after the cancel was confirmed must not be grantable"
    );
    assert!(view.auto_denied);
    assert!(run.answer(&id, "allow").is_err());
    assert_eq!(adapter.answers.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// A closed stdin wrote nothing, so the retry path must be reachable. Before
/// this, the only reachable write failure was "unknown", which blocks retries
/// forever.
#[test]
fn a_write_that_never_reached_the_cli_is_reported_as_unsent() {
    let process = crate::multicli::runtime::process::OwnedProcess::detached_for_tests();
    let error = process
        .write_line("{}")
        .expect_err("a detached process has no stdin");
    assert_eq!(error.code, crate::multicli::ErrorCode::Io);
    assert!(
        error.retryable,
        "nothing was written, so answering again is safe"
    );
}

/// `fail()` used to set `terminal_persisted` before writing the row, so a failed
/// commit was invisible to `stop`'s retry and the row stayed `starting`.
#[test]
fn fail_does_not_claim_persistence_before_the_row_is_written() {
    let service = service("r2-fail-commit");
    service
        .ensure_session(&crate::multicli::Session {
            id: "s-1".into(),
            agent_id: "claude".into(),
            host_id: "local-windows".into(),
            source_namespace: "claude:runtime:local-windows".into(),
            native_id: None,
            origin: crate::multicli::SessionOrigin::Managed,
            title: "t".into(),
        })
        .unwrap();
    let run = run(&service, "s-1", "r-1");
    run.begin().unwrap();
    let connection = rusqlite::Connection::open(service.store().database_path()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER synthetic_fail_failed BEFORE UPDATE OF execution_status ON runs \
             WHEN NEW.execution_status = 'failed' \
             BEGIN SELECT RAISE(ABORT,'synthetic temporary durable failure'); END;",
        )
        .unwrap();
    run.fail(&crate::multicli::AppError::new(
        crate::multicli::ErrorCode::Io,
        "synthetic spawn failure",
    ));
    let stored: String = connection
        .query_row(
            "SELECT execution_status FROM runs WHERE id='r-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored, "starting",
        "the row cannot say failed while the commit is still failing"
    );
    connection
        .execute_batch("DROP TRIGGER synthetic_fail_failed")
        .unwrap();
    run.stop("retry after storage recovers", Duration::ZERO);
    let stored: String = connection
        .query_row(
            "SELECT execution_status FROM runs WHERE id='r-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored, "failed",
        "stop must retry the failed terminal commit"
    );
}
