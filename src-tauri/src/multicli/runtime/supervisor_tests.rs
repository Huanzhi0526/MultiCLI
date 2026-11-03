//! Supervisor regressions for the remaining S05-R2 lifecycle gaps.
use super::*;
use crate::multicli::runtime::process::OwnedProcess;
use crate::multicli::{AppError, ErrorCode};

fn setup(timeout: Duration) -> (Arc<RuntimeRun>, Arc<CountingAdapter>, Arc<MultiCliService>) {
    let service = service("supervisor-lifecycle");
    service
        .ensure_session(&crate::multicli::Session {
            id: "session".into(),
            agent_id: "claude".into(),
            host_id: "local-windows".into(),
            source_namespace: "synthetic".into(),
            native_id: None,
            origin: crate::multicli::SessionOrigin::Managed,
            title: "test".into(),
        })
        .unwrap();
    let (run, adapter) = run_with_adapter_timeout(&service, "session", "run", timeout);
    run.begin().unwrap();
    (run, adapter, service)
}
fn start_turn(run: &RuntimeRun, id: &str, key: &str) {
    start(run, id, key);
}
fn permission(run: &RuntimeRun) -> String {
    run.accept(
        run.generation(),
        AdapterEvent::PermissionRequested {
            native_request_id: "native-permission".into(),
            title: "synthetic write".into(),
            detail: None,
            kind: "write".into(),
            options: vec![
                RuntimePermissionOption {
                    option_id: "allow".into(),
                    label: "allow".into(),
                    kind: "allow".into(),
                },
                RuntimePermissionOption {
                    option_id: "deny".into(),
                    label: "deny".into(),
                    kind: "deny".into(),
                },
            ],
            tool_call_id: Some("tool".into()),
            turn_id: Some("old".into()),
        },
        EventMeta::key("permission"),
    );
    run.view().permissions[0].permission_id.clone()
}
#[test]
fn supervisor_r2_exit_invalidates_pending_permissions() {
    let (run, adapter, _) = setup(Duration::from_secs(300));
    start_turn(&run, "old", "start");
    let id = permission(&run);
    run.accept(
        run.generation(),
        AdapterEvent::Exited {
            code: Some(0),
            reason: None,
        },
        EventMeta::key("exit"),
    );
    let answer = run.answer(&id, "allow");
    println!(
        "terminal={} answer_ok={} adapter_answers={} permissions={:?}",
        run.is_terminal(),
        answer.is_ok(),
        adapter.answers.load(Ordering::SeqCst),
        run.view().permissions
    );
    assert!(
        answer.is_err(),
        "terminal run must reject every old permission before invoking its adapter"
    );
    assert_eq!(adapter.answers.load(Ordering::SeqCst), 0);
}

struct SendFailAdapter;
impl Adapter for SendFailAdapter {
    fn native_session_id(&self) -> Option<String> {
        None
    }
    fn send_prompt(&self, _: &str) -> Result<(), AppError> {
        Err(AppError::new(ErrorCode::Io, "synthetic unsent prompt").with_retryable(true))
    }
    fn cancel_turn(&self, _: &str) -> Result<(), AppError> {
        Ok(())
    }
    fn answer_permission(&self, _: &str, _: &str, _: &str) -> Result<(), AppError> {
        Ok(())
    }
    fn shutdown(&self) {}
}

#[test]
fn supervisor_r2_missing_choice_never_invents_an_authorization() {
    let (run, _, _) = setup(Duration::from_secs(300));
    start_turn(&run, "old", "start");
    permission(&run);
    run.accept(
        run.generation(),
        AdapterEvent::ToolCall {
            tool_call_id: "tool".into(),
            name: "Read".into(),
            kind: "search".into(),
            input: None,
            permission_id: None,
        },
        EventMeta::key("tool"),
    );
    run.accept(
        run.generation(),
        AdapterEvent::PermissionResolved {
            native_request_id: "native-permission".into(),
            option_id: None,
            state: "answered".into(),
        },
        EventMeta::key("unspecified"),
    );
    run.accept(
        run.generation(),
        AdapterEvent::ToolUpdate {
            tool_call_id: "tool".into(),
            status: "completed".into(),
            output: Some("actual result".into()),
            is_error: false,
        },
        EventMeta::key("result"),
    );
    assert_eq!(
        run.view().tools[0].authorization,
        "unknown",
        "a peer receipt without a selected choice is neither a user allow nor a user deny"
    );
    assert_eq!(run.view().tools[0].execution, "executed");
}

#[test]
fn supervisor_r2_confirmed_turn_end_clears_old_denial_debt() {
    let (run, _, _) = setup(Duration::from_secs(300));
    start_turn(&run, "old", "start");
    permission(&run);
    run.cancel("cancel", Duration::ZERO).unwrap();
    assert!(run.has_pending_work());
    run.accept(
        run.generation(),
        AdapterEvent::TurnCompleted {
            turn_id: "old".into(),
            status: "cancelled".into(),
            reason: None,
        },
        EventMeta::key("ack"),
    );
    assert!(
        !run.has_pending_work(),
        "a confirmed ended turn must not retry ids that the peer already cleared"
    );
}

#[test]
fn supervisor_r2_outbound_prompt_and_decision_survive_restart() {
    let (run, _, service) = setup(Duration::from_secs(300));
    run.send("durable Chinese sentinel 中文").unwrap();
    start_turn(&run, "old", "start");
    let id = permission(&run);
    run.answer(&id, "deny").unwrap();
    // Open a second connection, as history/restart does; memory views do not
    // prove that either outbound record reached the store.
    let connection = rusqlite::Connection::open(service.store().database_path()).unwrap();
    let users:i64=connection.query_row("SELECT count(*) FROM messages WHERE kind='message/completed' AND json_extract(content_json,'$.role')='user' AND json_extract(content_json,'$.text')='durable Chinese sentinel 中文'",[],|r|r.get(0)).unwrap();
    let decisions:i64=connection.query_row("SELECT count(*) FROM messages WHERE kind='permission/resolved' AND json_extract(content_json,'$.selectedOptionId')='deny'",[],|r|r.get(0)).unwrap();
    println!("durable_users={} durable_decisions={}", users, decisions);
    assert_eq!((users,decisions),(1,1),"outbound user text and the permission decision must both be durable without relying on peer echoes");
}

#[test]
fn supervisor_r2_unsent_prompt_cannot_leave_a_phantom_running_turn() {
    let (run, adapter, _) = setup(Duration::from_secs(300));
    run.attach_adapter(
        Arc::new(SendFailAdapter),
        Arc::new(OwnedProcess::detached_for_tests()),
    );
    assert!(run.send("synthetic prompt not written").is_err());
    let failed = run.view();
    run.attach_adapter(adapter, Arc::new(OwnedProcess::detached_for_tests()));
    let retry = run.send("retry after transport recovers");
    println!("failed_turn={:?} retry_ok={}", failed.turn, retry.is_ok());
    assert!(
        retry.is_ok(),
        "a definitely unsent prompt must not lock the conversation into a phantom running turn"
    );
}

struct WireBlockAdapter {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    answers: std::sync::Mutex<Vec<(String, String)>>,
}
impl Adapter for WireBlockAdapter {
    fn native_session_id(&self) -> Option<String> {
        None
    }
    fn send_prompt(&self, _: &str) -> Result<(), AppError> {
        Ok(())
    }
    fn cancel_turn(&self, _: &str) -> Result<(), AppError> {
        Ok(())
    }
    fn answer_permission(&self, id: &str, option: &str, _: &str) -> Result<(), AppError> {
        if id == "native-permission" {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(9))
                .unwrap();
        }
        self.answers
            .lock()
            .unwrap()
            .push((id.into(), option.into()));
        Ok(())
    }
    fn shutdown(&self) {}
}

#[test]
fn supervisor_r2_answer_rechecks_deadline_after_waiting_for_wire() {
    let (run, _, _) = setup(Duration::from_secs(5));
    start_turn(&run, "old", "start");
    let first = permission(&run);
    run.accept(
        run.generation(),
        AdapterEvent::PermissionRequested {
            native_request_id: "second".into(),
            title: "second request".into(),
            detail: None,
            kind: "write".into(),
            options: vec![
                RuntimePermissionOption {
                    option_id: "allow".into(),
                    label: "allow".into(),
                    kind: "allow".into(),
                },
                RuntimePermissionOption {
                    option_id: "deny".into(),
                    label: "deny".into(),
                    kind: "deny".into(),
                },
            ],
            tool_call_id: None,
            turn_id: Some("old".into()),
        },
        EventMeta::key("second"),
    );
    let second = run.view().permissions[1].permission_id.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let adapter = Arc::new(WireBlockAdapter {
        entered: entered_tx,
        release: std::sync::Mutex::new(release_rx),
        answers: std::sync::Mutex::new(Vec::new()),
    });
    run.attach_adapter(
        adapter.clone(),
        Arc::new(OwnedProcess::detached_for_tests()),
    );
    let one = run.clone();
    let first_worker = std::thread::spawn(move || one.answer(&first, "allow"));
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let two = run.clone();
    let second_worker = std::thread::spawn(move || two.answer(&second, "allow"));
    std::thread::sleep(Duration::from_millis(5100));
    release_tx.send(()).unwrap();
    let _ = first_worker.join().unwrap();
    let result = second_worker.join().unwrap();
    println!(
        "second_answer_ok={} wire={:?}",
        result.is_ok(),
        adapter.answers.lock().unwrap()
    );
    assert!(result.is_err(),"the second approval expired while waiting for another write and must never reach the adapter");
    assert!(!adapter
        .answers
        .lock()
        .unwrap()
        .iter()
        .any(|(id, option)| id == "second" && option == "allow"));
}
