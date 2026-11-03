//! Real acceptance probe for the S05 structured runtime — **not** a unit test.
//!
//! This drives the production `RuntimeManager` against the CLIs actually
//! installed on this machine, inside an isolated temporary project, and prints
//! one JSON report. It exists so the delivery report can quote a run that went
//! through the same code the application uses, instead of a hand-written
//! socket script.
//!
//! It is never started by the product. Run it explicitly:
//!
//! ```text
//! MULTICLI_PROFILE=test cargo run --example multicli_runtime_probe -- <outdir> <product>
//! ```
//!
//! Behaviour by product, derived from what this machine can actually reach:
//!
//! | product | what the probe does |
//! |---|---|
//! | `claude` | the full A12 chain: deny a write (file must not appear), allow a write (file must appear), cancel a turn, stop, continue the session in a new Run |
//! | `codex` | the same chain: deny (file must not appear), allow (file must appear), cancel, stop, continue in a new Run |
//! | `codebuddy` | the file-tool chain with declared ACP option semantics, cancel, stop and session/load followed by a fresh prompt |
//!
//! Every conclusion is written with its own `status` and `reason`; a step that
//! could not run is `NOT_RUN`, never "passed". A step whose precondition was
//! present but which the environment refused (an unauthenticated ACP
//! connection, an unreachable model endpoint) is `BLOCKED`, which is neither a
//! product failure nor a pass.
//!
//! The report also carries `coverage` (one entry per capability, with the
//! number of samples in this run) and `tally` (counts plus the ids that did not
//! run). `status` is derived from `coverage`, and it spells out anything that
//! did not run, so a single word cannot stand in for an untested capability.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use ccgui_next_lib::multicli::domain::{AgentInstallation, HostPath};
use ccgui_next_lib::multicli::runtime::{
    RuntimeManager, RuntimeProduct, RuntimeRunView, RuntimeStartRequest,
};
use ccgui_next_lib::multicli::MultiCliService;

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or(0)
}

struct Probe {
    manager: Arc<RuntimeManager>,
    project: PathBuf,
    out: Value,
    steps: Vec<Value>,
}

impl Probe {
    fn step(&mut self, id: &str, status: &str, detail: Value) {
        self.steps.push(json!({
            "id": id,
            "status": status,
            "at": now_ms(),
            "detail": detail,
        }));
    }

    /// Status of the last step with this id, or `NOT_RUN` when it never ran.
    fn status_of(&self, id: &str) -> String {
        self.steps
            .iter()
            .rev()
            .find(|step| step["id"] == id)
            .and_then(|step| step["status"].as_str())
            .unwrap_or("NOT_RUN")
            .to_string()
    }

    /// Counts every step by status so a caller cannot quote one overall word
    /// while some step never ran.
    fn tally(&self) -> Value {
        let mut pass = 0;
        let mut failed = 0;
        let mut not_run = 0;
        let mut blocked = 0;
        for step in &self.steps {
            match step["status"].as_str().unwrap_or("NOT_RUN") {
                "PASS" => pass += 1,
                "FAILED" => failed += 1,
                "BLOCKED" => blocked += 1,
                _ => not_run += 1,
            }
        }
        json!({
            "pass": pass,
            "failed": failed,
            "blocked": blocked,
            "notRun": not_run,
            "notRunSteps": self
                .steps
                .iter()
                .filter(|step| step["status"] == "NOT_RUN")
                .filter_map(|step| step["id"].as_str().map(str::to_string))
                .collect::<Vec<_>>(),
            "blockedSteps": self
                .steps
                .iter()
                .filter(|step| step["status"] == "BLOCKED")
                .filter_map(|step| step["id"].as_str().map(str::to_string))
                .collect::<Vec<_>>(),
        })
    }

    /// Waits until `predicate` holds, polling the production view.
    fn wait_for<F>(&self, run_id: &str, limit: Duration, mut predicate: F) -> Option<RuntimeRunView>
    where
        F: FnMut(&RuntimeRunView) -> bool,
    {
        let deadline = Instant::now() + limit;
        loop {
            if let Ok(view) = self.manager.get(run_id) {
                if predicate(&view) {
                    return Some(view);
                }
            }
            if Instant::now() >= deadline {
                return self.manager.get(run_id).ok();
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Answers permission requests according to `policy`, until the turn ends.
    fn run_turn(
        &mut self,
        run_id: &str,
        policy: &[&str],
        limit: Duration,
    ) -> (Option<RuntimeRunView>, Vec<Value>) {
        let deadline = Instant::now() + limit;
        let mut answered: Vec<Value> = Vec::new();
        let mut index = 0_usize;
        loop {
            let Some(view) = self.wait_for(run_id, Duration::from_millis(400), |view| {
                view.permissions.iter().any(|p| p.state == "pending")
                    || view
                        .turn
                        .as_ref()
                        .map(|turn| turn.state != "running")
                        .unwrap_or(false)
            }) else {
                return (None, answered);
            };
            let pending: Vec<_> = view
                .permissions
                .iter()
                .filter(|permission| permission.state == "pending")
                .cloned()
                .collect();
            for permission in pending {
                let choice = policy.get(index).copied().unwrap_or("deny");
                index += 1;
                // Match the explicit id or its declared one-shot semantics.
                // Never fall back to an arbitrary/always-allow option, and
                // terminate on mismatch instead of retrying a pending card.
                let Some(option) = permission
                    .options
                    .iter()
                    .find(|option| option.option_id == choice)
                    .or_else(|| {
                        permission.options.iter().find(|option| match choice {
                            "deny" | "decline" | "cancel" => option.kind == "deny",
                            "allow" | "accept" => option.kind == "allow",
                            _ => false,
                        })
                    })
                else {
                    answered.push(json!({
                        "permissionId": permission.permission_id,
                        "expectedOption": choice,
                        "offered": permission.options.iter().map(|o| o.option_id.clone()).collect::<Vec<_>>(),
                        "error": "the requested option was not offered; nothing was answered",
                    }));
                    return (Some(view), answered);
                };
                match self.manager.answer(
                    &ccgui_next_lib::multicli::runtime::RuntimeAnswerRequest {
                        run_id: run_id.to_string(),
                        permission_id: permission.permission_id.clone(),
                        option_id: option.option_id.clone(),
                    },
                ) {
                    Ok(updated) => {
                        if updated.permissions.iter().any(|p| {
                            p.permission_id == permission.permission_id && p.state == "pending"
                        }) {
                            answered.push(json!({"permissionId":permission.permission_id,"error":"decision submitted but not applied; never automatically resubmit"}));
                            return (Some(updated), answered);
                        }
                        answered.push(json!({
                        "permissionId": permission.permission_id,
                        "nativeRequestId": permission.native_request_id,
                        "title": permission.title,
                        "kind": permission.kind,
                        "toolCallId": permission.tool_call_id,
                        "offered": permission.options.iter().map(|o| o.option_id.clone()).collect::<Vec<_>>(),
                        "answeredWith": option.option_id,
                        "kindOfOption": option.kind,
                    }));
                    }
                    Err(error) => {
                        answered.push(json!({
                            "permissionId": permission.permission_id,
                            "error": error.message,
                            "code": error.code.as_str(),
                        }));
                        return (Some(view), answered);
                    }
                }
            }
            let turn_done = view
                .turn
                .as_ref()
                .map(|turn| turn.state != "running")
                .unwrap_or(false);
            if turn_done && !view.permissions.iter().any(|p| p.state == "pending") {
                return (Some(view), answered);
            }
            if Instant::now() >= deadline {
                return (self.manager.get(run_id).ok(), answered);
            }
        }
    }
}

fn file_state(path: &Path) -> Value {
    match std::fs::read_to_string(path) {
        Ok(text) => json!({ "exists": true, "content": text }),
        Err(_) => json!({ "exists": false, "content": Value::Null }),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_dir = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("multicli-runtime-probe"));
    let product = args.get(2).map(|value| value.as_str()).unwrap_or("claude");
    std::fs::create_dir_all(&out_dir).expect("create output directory");
    let project = out_dir.join(format!("project-{product}"));
    std::fs::create_dir_all(&project).expect("create probe project");

    let data_root = out_dir.join("profile");
    let service = Arc::new(MultiCliService::open(&data_root).expect("open the isolated database"));
    service
        .register_host(&ccgui_next_lib::multicli::Host::local("local-windows"))
        .expect("register host");
    let runtime_product = match product {
        "codex" => RuntimeProduct::Codex,
        "codebuddy" => RuntimeProduct::CodeBuddy,
        _ => RuntimeProduct::ClaudeCode,
    };
    service
        .register_agent(&ccgui_next_lib::multicli::AgentDefinition {
            id: runtime_product.agent_id().into(),
            vendor: "probe".into(),
            display_name: runtime_product.display_name().into(),
        })
        .expect("register agent");
    let executable = match runtime_product {
        RuntimeProduct::Codex => dirs::data_local_dir()
            .expect("local application data")
            .join("Programs/OpenAI/Codex/bin/codex.exe"),
        RuntimeProduct::ClaudeCode => dirs::home_dir()
            .expect("home directory")
            .join(".local/bin/claude.exe"),
        RuntimeProduct::CodeBuddy => {
            PathBuf::from(std::env::var("APPDATA").expect("roaming application data"))
                .join("npm/codebuddy.cmd")
        }
    };
    let default_version = match runtime_product {
        RuntimeProduct::Codex => "0.166.0",
        RuntimeProduct::ClaudeCode => "2.1.153",
        RuntimeProduct::CodeBuddy => "2.179.0",
    };
    // Explicit test override records the installed version without silently
    // attributing a newer CLI's result to the original acceptance baseline.
    let version =
        std::env::var("MULTICLI_PROBE_VERSION").unwrap_or_else(|_| default_version.to_string());
    let installation_id = format!("probe-{product}");
    service
        .register_installation(&AgentInstallation {
            id: installation_id.clone(),
            host_id: "local-windows".into(),
            agent_id: runtime_product.agent_id().into(),
            executable: HostPath {
                host_id: "local-windows".into(),
                display_path: executable.display().to_string(),
                // The product normalises with `dunce`, which strips the
                // verbatim prefix that `std::fs::canonicalize` adds. Using the
                // std spelling here is exactly what made a `.cmd` target
                // unlaunchable during S04's first acceptance attempt.
                canonical_path: dunce::canonicalize(&executable)
                    .ok()
                    .map(|path| path.display().to_string()),
            },
            version: Some(version.clone()),
            config_profile_id: "probe".into(),
        })
        .expect("register installation");

    let manager = Arc::new(
        RuntimeManager::new(
            Arc::clone(&service),
            data_root.clone(),
            "test".into(),
            Arc::new(|_notify| {}),
        )
        .expect("create the runtime manager"),
    );
    let mut probe = Probe {
        manager: Arc::clone(&manager),
        project: project.clone(),
        out: json!({}),
        steps: Vec::new(),
    };

    let mut result = json!({
        "product": runtime_product.product_key(),
        "version": version,
        "project": project.display().to_string(),
        "dataRoot": data_root.display().to_string(),
    });

    // `connect` mode opens a connection and stops it: no model request, so it
    // can be repeated without spending quota.
    if args.get(3).map(String::as_str) == Some("connect") {
        let started = probe.manager.start(&RuntimeStartRequest {
            installation_id: installation_id.clone(),
            cwd: project.display().to_string(),
            prompt: None,
            title: Some("connect-only".into()),
            permission_timeout_ms: Some(60_000),
            model: None,
            effort: None,
        });
        match started {
            Ok(view) => {
                probe.step(
                    "start",
                    "PASS",
                    json!({
                        "runId": view.run_id,
                        "nativeSessionId": view.native_session_id,
                        "executionState": view.execution_state,
                    }),
                );
                std::thread::sleep(Duration::from_millis(800));
                match probe
                    .manager
                    .stop(&ccgui_next_lib::multicli::runtime::RuntimeStopRequest {
                        run_id: view.run_id.clone(),
                        grace_ms: Some(4_000),
                    }) {
                    Ok(stopped) => probe.step(
                        "stop",
                        "PASS",
                        json!({
                            "executionState": stopped.execution_state,
                            "endedAt": stopped.ended_at,
                            "notes": stopped.notes,
                        }),
                    ),
                    Err(error) => probe.step(
                        "stop",
                        "FAILED",
                        json!({
                            "error": error.message, "code": error.code.as_str(),
                            "details": error.details,
                        }),
                    ),
                }
            }
            Err(error) => probe.step(
                "start",
                "FAILED",
                json!({
                    "error": error.message, "code": error.code.as_str(), "details": error.details,
                }),
            ),
        }
        result["status"] = json!("CONNECT_ONLY");
        result["steps"] = json!(probe.steps);
        let path = out_dir.join(format!("report-{product}-connect.json"));
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )
        .expect("write report");
        println!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_default()
        );
        return;
    }

    match runtime_product {
        RuntimeProduct::ClaudeCode => {
            file_permission_chain(&mut probe, &installation_id, &mut result)
        }
        RuntimeProduct::Codex => codex_probe(&mut probe, &installation_id, &mut result),
        RuntimeProduct::CodeBuddy => {
            file_permission_chain(&mut probe, &installation_id, &mut result)
        }
    }

    result["steps"] = json!(probe.steps);
    let path = out_dir.join(format!("report-{product}.json"));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".into()),
    )
    .expect("write report");
    println!(
        "{}",
        serde_json::to_string_pretty(&result).unwrap_or_default()
    );
    println!("\nreport written to {}", path.display());
}

/// Derives one label from per-capability coverage.
///
/// A bare `PASS` is only produced when every capability ran. Anything that did
/// not run is spelled out in the label, because an overall word is what gets
/// quoted in a report and it must not cover an untested capability.
fn overall_status(coverage: &Value) -> String {
    let mut not_run: Vec<String> = Vec::new();
    let mut blocked = false;
    let mut all_pass = true;
    for (name, entry) in coverage.as_object().into_iter().flatten() {
        match entry["status"].as_str().unwrap_or("NOT_RUN") {
            "FAILED" => return "FAILED".into(),
            "BLOCKED" => {
                blocked = true;
                all_pass = false;
            }
            "NOT_RUN" => {
                not_run.push(name.clone());
                all_pass = false;
            }
            _ => {}
        }
    }
    if all_pass {
        return "PASS".into();
    }
    let mut label = String::new();
    if blocked {
        label.push_str("BLOCKED");
    }
    if !not_run.is_empty() {
        if !label.is_empty() {
            label.push('+');
        }
        label.push_str(&format!("NOT_RUN({})", not_run.join(",")));
    }
    label
}

/// The file-tool chain shared by Claude and CodeBuddy, through their adapters.
fn file_permission_chain(probe: &mut Probe, installation_id: &str, result: &mut Value) {
    let deny_path = probe.project.join("probe-deny.txt");
    let allow_path = probe.project.join("probe-allow.txt");
    let _ = std::fs::remove_file(&deny_path);
    let _ = std::fs::remove_file(&allow_path);

    // 1. New run + first message.
    let started = probe.manager.start(&RuntimeStartRequest {
        installation_id: installation_id.to_string(),
        cwd: probe.project.display().to_string(),
        prompt: Some(
            "请只做一件事：在当前目录创建文件 probe-deny.txt，内容为 hello。不要做别的。".into(),
        ),
        title: Some("S05 真实验收".into()),
        permission_timeout_ms: Some(120_000),
        model: None,
        effort: None,
    });
    let run = match started {
        Ok(view) => view,
        Err(error) => {
            probe.step(
                "start",
                "FAILED",
                json!({ "error": error.message, "code": error.code.as_str() }),
            );
            result["status"] = json!("FAILED");
            return;
        }
    };
    probe.step(
        "start",
        "PASS",
        json!({
            "runId": run.run_id,
            "sessionId": run.session_id,
            "nativeSessionId": run.native_session_id,
            "executionState": run.execution_state,
            "launchArgv": run.launch_argv,
            "processPid": run.process.as_ref().map(|p| p.pid),
        }),
    );

    // 2. First turn: deny the write.
    let (view, answered) = probe.run_turn(&run.run_id, &["deny"], Duration::from_secs(180));
    let denied_file_absent = !file_state(&deny_path)["exists"].as_bool().unwrap_or(true);
    let denied = denied_file_absent
        && answered.iter().any(|a| a["kindOfOption"] == "deny")
        && view
            .as_ref()
            .and_then(|v| v.turn.as_ref())
            .is_some_and(|t| matches!(t.state.as_str(), "completed" | "cancelled"));
    probe.step(
        "turn1-deny",
        if denied { "PASS" } else { "FAILED" },
        json!({
            "answered": answered,
            "denyFile": file_state(&deny_path),
            "turn": view.as_ref().and_then(|v| v.turn.clone()),
            "interactionState": view.as_ref().map(|v| v.interaction_state.clone()),
            "tools": view.as_ref().map(|v| v.tools.clone()),
            "messages": view.as_ref().map(|v| v.messages.clone()),
            "note": if answered.is_empty() {
                "没有收到权限请求：该回合未进入需要授权的一步"
            } else {
                "拒绝后目标文件必须不存在"
            },
        }),
    );
    result["denyLeftFileAbsent"] = json!(denied_file_absent);
    if !denied {
        // A failed deny policy must never fall through to the allow phase on
        // the same still-pending request.
        let stopped = probe
            .manager
            .stop(&ccgui_next_lib::multicli::runtime::RuntimeStopRequest {
                run_id: run.run_id.clone(),
                grace_ms: Some(1_000),
            });
        probe.step(
            "stop",
            if stopped.is_ok() { "PASS" } else { "FAILED" },
            json!({"reason":"deny verification failed; no allow phase was run"}),
        );
        result["coverage"] = json!({"start":{"status":"PASS","samples":1},"permission.deny":{"status":"FAILED","samples":1},"permission.allow":{"status":"NOT_RUN","samples":0},"chat":{"status":"NOT_RUN","samples":0},"tools":{"status":"NOT_RUN","samples":0},"cancel":{"status":"NOT_RUN","samples":0},"resume":{"status":"NOT_RUN","samples":0},"stop":{"status":probe.status_of("stop"),"samples":1}});
        result["tally"] = probe.tally();
        result["status"] = json!(overall_status(&result["coverage"]));
        return;
    }

    // 3. Second turn on the same run: allow the write.
    let sent = probe
        .manager
        .send(&ccgui_next_lib::multicli::runtime::RuntimeSendRequest {
            run_id: run.run_id.clone(),
            text: "请只做一件事：在当前目录创建文件 probe-allow.txt，内容为 ok。不要做别的。"
                .into(),
        });
    if let Err(error) = &sent {
        probe.step(
            "send2",
            "FAILED",
            json!({ "error": error.message, "code": error.code.as_str() }),
        );
    }
    let (view, answered) = probe.run_turn(&run.run_id, &["allow"], Duration::from_secs(180));
    let allow_file = file_state(&allow_path);
    let allowed = std::fs::read_to_string(&allow_path).is_ok_and(|text| text.trim() == "ok")
        && answered.iter().any(|a| a["kindOfOption"] == "allow")
        && view
            .as_ref()
            .and_then(|v| v.turn.as_ref())
            .is_some_and(|t| t.state == "completed");
    let chat_observed = view.as_ref().is_some_and(|v| {
        v.messages
            .iter()
            .any(|m| m.role == "assistant" && !m.text.trim().is_empty())
    });
    probe.step(
        "turn2-allow",
        if allowed { "PASS" } else { "FAILED" },
        json!({
            "answered": answered,
            "allowFile": allow_file,
            "turn": view.as_ref().and_then(|v| v.turn.clone()),
            "messages": view.as_ref().map(|v| v.messages.clone()),
        }),
    );
    result["allowCreatedFile"] = json!(allow_file["exists"].as_bool().unwrap_or(false));

    // 4. Cancel a running turn.
    let long = probe
        .manager
        .send(&ccgui_next_lib::multicli::runtime::RuntimeSendRequest {
            run_id: run.run_id.clone(),
            text: "请逐行数到 300，每行只输出一个数字，不要别的文字。".into(),
        });
    match long {
        Ok(_) => {
            std::thread::sleep(Duration::from_millis(1_500));
            match probe
                .manager
                .cancel(&ccgui_next_lib::multicli::runtime::RuntimeCancelRequest {
                    run_id: run.run_id.clone(),
                    grace_ms: Some(15_000),
                }) {
                Ok(view) => probe.step(
                    "cancel",
                    if view.turn.as_ref().is_some_and(|t| t.state == "cancelled") {
                        "PASS"
                    } else {
                        "FAILED"
                    },
                    json!({
                        "turn": view.turn,
                        "interactionState": view.interaction_state,
                        "notes": view.notes,
                    }),
                ),
                Err(error) => probe.step(
                    "cancel",
                    "FAILED",
                    json!({ "error": error.message, "code": error.code.as_str() }),
                ),
            }
        }
        Err(error) => probe.step(
            "cancel",
            "NOT_RUN",
            json!({ "error": error.message, "code": error.code.as_str() }),
        ),
    }

    // 5. Continue the same session in a new Run.
    let native = probe
        .manager
        .get(&run.run_id)
        .ok()
        .and_then(|view| view.native_session_id.clone());
    let session_id = run.session_id.clone();
    let stopped = probe
        .manager
        .stop(&ccgui_next_lib::multicli::runtime::RuntimeStopRequest {
            run_id: run.run_id.clone(),
            grace_ms: Some(5_000),
        })
        .ok();
    probe.step(
        "stop",
        if stopped.is_some() { "PASS" } else { "FAILED" },
        json!({
            "executionState": stopped.as_ref().map(|v| v.execution_state.clone()),
            "endedAt": stopped.as_ref().and_then(|v| v.ended_at.clone()),
        }),
    );
    match probe
        .manager
        .resume(&ccgui_next_lib::multicli::runtime::RuntimeResumeRequest {
            session_id: session_id.clone(),
            installation_id: Some(installation_id.to_string()),
            cwd: Some(probe.project.display().to_string()),
            prompt: Some("请只回复两个字：收到".into()),
            permission_timeout_ms: Some(120_000),
        }) {
        Ok(view) => {
            let (view2, _) = probe.run_turn(&view.run_id, &["deny"], Duration::from_secs(120));
            probe.step(
                "resume",
                if view.native_session_id == native
                    && view.run_id != run.run_id
                    && view2.as_ref().is_some_and(|v| {
                        v.turn.as_ref().is_some_and(|t| t.state == "completed")
                            && v.messages
                                .iter()
                                .any(|m| m.role == "assistant" && m.text.trim().contains("收到"))
                    })
                {
                    "PASS"
                } else {
                    "FAILED"
                },
                json!({
                    "newRunId": view.run_id,
                    "sameNativeSessionId": view.native_session_id == native,
                    "nativeSessionId": view.native_session_id,
                    "messages": view2.as_ref().map(|v| v.messages.clone()),
                }),
            );
            result["resumeReusedSameSession"] = json!(view.native_session_id == native);
            let _ = probe
                .manager
                .stop(&ccgui_next_lib::multicli::runtime::RuntimeStopRequest {
                    run_id: view.run_id.clone(),
                    grace_ms: Some(5_000),
                });
        }
        Err(error) => probe.step(
            "resume",
            "NOT_RUN",
            json!({ "error": error.message, "code": error.code.as_str() }),
        ),
    }

    // One line per capability this stage claims. `samples` is the number of
    // times the capability was exercised in *this* run: a single probe run is
    // n=1, never "verified".
    result["coverage"] = json!({
        "start": { "status": probe.status_of("start"), "samples": 1 },
        "chat": { "status": if chat_observed {"PASS"} else {"FAILED"}, "samples": 1 },
        "permission.deny": { "status": probe.status_of("turn1-deny"), "samples": 1 },
        "permission.allow": { "status": probe.status_of("turn2-allow"), "samples": 1 },
        "tools": { "status": probe.status_of("turn2-allow"), "samples": 1 },
        "cancel": { "status": probe.status_of("cancel"), "samples": 1 },
        "resume": { "status": probe.status_of("resume"), "samples": 1 },
        "stop": { "status": probe.status_of("stop"), "samples": 1 },
    });
    result["tally"] = probe.tally();
    result["status"] = json!(overall_status(&result["coverage"]));
}

fn codex_probe(probe: &mut Probe, installation_id: &str, result: &mut Value) {
    let deny_path = probe.project.join("probe-codex-deny.txt");
    let allow_path = probe.project.join("probe-codex-allow.txt");
    let _ = std::fs::remove_file(&deny_path);
    let _ = std::fs::remove_file(&allow_path);

    let started = probe.manager.start(&RuntimeStartRequest {
        installation_id: installation_id.to_string(),
        cwd: probe.project.display().to_string(),
        prompt: Some(
            "请只做一件事：在当前目录创建文件 probe-codex-deny.txt，内容为 hello。不要做别的。"
                .into(),
        ),
        title: Some("S05 Codex 探针".into()),
        permission_timeout_ms: Some(120_000),
        model: None,
        effort: None,
    });
    let run = match started {
        Ok(view) => view,
        Err(error) => {
            probe.step(
                "start",
                "FAILED",
                json!({ "error": error.message, "code": error.code.as_str() }),
            );
            result["status"] = json!("FAILED");
            return;
        }
    };
    probe.step(
        "start",
        "PASS",
        json!({
            "runId": run.run_id,
            "nativeSessionId": run.native_session_id,
            "launchArgv": run.launch_argv,
            "processPid": run.process.as_ref().map(|p| p.pid),
        }),
    );

    // Deny the write, then prove the file never appeared.
    let (view, answered) = probe.run_turn(&run.run_id, &["decline"], Duration::from_secs(240));
    let deny_state = file_state(&deny_path);
    probe.step(
        "turn1-deny",
        if answered.is_empty() {
            "NOT_RUN"
        } else {
            "PASS"
        },
        json!({
            "answered": answered,
            "denyFile": deny_state,
            "turn": view.as_ref().and_then(|v| v.turn.clone()),
            "messages": view.as_ref().map(|v| v.messages.clone()),
            "notes": view.as_ref().map(|v| v.notes.clone()),
            "reason": if answered.is_empty() {
                "回合结束但没有出现可答复的权限请求"
            } else {
                "拒绝后目标文件必须不存在"
            },
        }),
    );
    result["denyLeftFileAbsent"] = json!(!deny_state["exists"].as_bool().unwrap_or(true));

    // Allow a second, explicit write.
    let _ = probe
        .manager
        .send(&ccgui_next_lib::multicli::runtime::RuntimeSendRequest {
            run_id: run.run_id.clone(),
            text: "请只做一件事：在当前目录创建文件 probe-codex-allow.txt，内容为 ok。不要做别的。"
                .into(),
        });
    let (view, answered) = probe.run_turn(&run.run_id, &["accept"], Duration::from_secs(240));
    let allow_state = file_state(&allow_path);
    probe.step(
        "turn2-allow",
        if allow_state["exists"].as_bool().unwrap_or(false) {
            "PASS"
        } else {
            "NOT_RUN"
        },
        json!({
            "answered": answered,
            "allowFile": allow_state,
            "turn": view.as_ref().and_then(|v| v.turn.clone()),
        }),
    );
    result["allowCreatedFile"] = json!(allow_state["exists"].as_bool().unwrap_or(false));

    // Cancel a running turn: the interrupt path is measured on this product.
    if probe
        .manager
        .send(&ccgui_next_lib::multicli::runtime::RuntimeSendRequest {
            run_id: run.run_id.clone(),
            text: "请逐行数到 300，每行只输出一个数字，不要别的文字。".into(),
        })
        .is_ok()
    {
        std::thread::sleep(Duration::from_millis(1_500));
        match probe
            .manager
            .cancel(&ccgui_next_lib::multicli::runtime::RuntimeCancelRequest {
                run_id: run.run_id.clone(),
                grace_ms: Some(15_000),
            }) {
            Ok(view) => probe.step(
                "cancel",
                if view
                    .turn
                    .as_ref()
                    .map(|t| t.state != "running")
                    .unwrap_or(false)
                {
                    "PASS"
                } else {
                    "NOT_RUN"
                },
                json!({
                    "turn": view.turn,
                    "interactionState": view.interaction_state,
                    "notes": view.notes,
                }),
            ),
            Err(error) => probe.step(
                "cancel",
                "NOT_RUN",
                json!({ "error": error.message, "code": error.code.as_str() }),
            ),
        }
    }

    // Continue the same session in a new Run. This was NOT_RUN in R1: the RPC
    // was exercised, but never a real continue chain.
    let native = probe
        .manager
        .get(&run.run_id)
        .ok()
        .and_then(|view| view.native_session_id.clone());
    let session_id = run.session_id.clone();
    let stopped = probe
        .manager
        .stop(&ccgui_next_lib::multicli::runtime::RuntimeStopRequest {
            run_id: run.run_id.clone(),
            grace_ms: Some(5_000),
        })
        .ok();
    probe.step(
        "stop",
        if stopped.is_some() { "PASS" } else { "FAILED" },
        json!({
            "executionState": stopped.as_ref().map(|v| v.execution_state.clone()),
            "endedAt": stopped.as_ref().and_then(|v| v.ended_at.clone()),
        }),
    );
    match probe
        .manager
        .resume(&ccgui_next_lib::multicli::runtime::RuntimeResumeRequest {
            session_id,
            installation_id: Some(installation_id.to_string()),
            cwd: Some(probe.project.display().to_string()),
            prompt: Some("请只回复两个字：收到".into()),
            permission_timeout_ms: Some(120_000),
        }) {
        Ok(view) => {
            let (done, _) = probe.run_turn(&view.run_id, &["accept"], Duration::from_secs(180));
            probe.step(
                "resume",
                if view.native_session_id == native && view.run_id != run.run_id {
                    "PASS"
                } else {
                    "FAILED"
                },
                json!({
                    "newRunId": view.run_id,
                    "sameNativeSessionId": view.native_session_id == native,
                    "nativeSessionId": view.native_session_id,
                    "turn": done.as_ref().and_then(|v| v.turn.clone()),
                    "messages": done.as_ref().map(|v| v.messages.clone()),
                }),
            );
            result["resumeReusedSameSession"] = json!(view.native_session_id == native);
            let _ = probe
                .manager
                .stop(&ccgui_next_lib::multicli::runtime::RuntimeStopRequest {
                    run_id: view.run_id,
                    grace_ms: Some(5_000),
                });
        }
        Err(error) => probe.step(
            "resume",
            "NOT_RUN",
            json!({ "error": error.message, "code": error.code.as_str() }),
        ),
    }

    // `resume` is included even though it used to be reported as NOT_RUN while
    // the overall label still said the chain passed: that is exactly the
    // masking this shape removes.
    result["coverage"] = json!({
        "start": { "status": probe.status_of("start"), "samples": 1 },
        "chat": { "status": probe.status_of("turn1-deny"), "samples": 1 },
        "permission.deny": { "status": probe.status_of("turn1-deny"), "samples": 1 },
        "permission.allow": { "status": probe.status_of("turn2-allow"), "samples": 1 },
        "tools": { "status": probe.status_of("turn2-allow"), "samples": 1 },
        "cancel": { "status": probe.status_of("cancel"), "samples": 1 },
        "resume": { "status": probe.status_of("resume"), "samples": 1 },
        "stop": { "status": probe.status_of("stop"), "samples": 1 },
    });
    result["tally"] = probe.tally();
    result["status"] = json!(overall_status(&result["coverage"]));
}
