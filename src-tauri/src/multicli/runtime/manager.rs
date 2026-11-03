//! Runtime registry: target resolution, Run creation, resume and the
//! permission sweeper.
//!
//! Boundary rules this layer enforces:
//!
//! - only a registered installation with a canonical executable can be started;
//! - a run is recorded as `starting` **before** anything is spawned, so a
//!   failed spawn still leaves a real, explainable Run instead of a silent gap;
//! - continuing a historical Session always produces a **new Run** on the same
//!   Session, never a second Session and never a second owner of the old
//!   process;
//! - a known-live native session is refused before anything starts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::json;
use uuid::Uuid;

use crate::multicli::domain::{
    AgentDefinition, AgentInstallation, HostPath, Session, SessionOrigin, SessionWorkspace,
    SessionWorkspaceRole,
};
use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::history::resume::resolve_resume_target;
use crate::multicli::terminal::launch::LaunchPaths;
use crate::multicli::terminal::model::LaunchChain;
use crate::multicli::MultiCliService;

use super::adapters::{claude, codebuddy, codex, Emitter};
use super::launch::{cwd_path, plan, LaunchRequest};
use super::model::{
    installation_path, RuntimeAnswerRequest, RuntimeCancelRequest, RuntimeCapabilitiesView,
    RuntimeCapabilityView, RuntimeEventPage, RuntimeListView, RuntimeProduct, RuntimeReadRequest,
    RuntimeResumeRequest, RuntimeRunView, RuntimeSendRequest, RuntimeStartRequest,
    RuntimeStopRequest, MAX_RUNS, RUNTIME_NOTE, RUNTIME_SCHEMA_VERSION,
};
use super::process::{OwnedProcess, ProcessSpec};
use super::run::{NotifyFn, RuntimeRun};

pub struct RuntimeManager {
    host_id: String,
    profile: String,
    service: Arc<MultiCliService>,
    data_root: PathBuf,
    launch_paths: LaunchPaths,
    runs: Mutex<BTreeMap<String, Arc<RuntimeRun>>>,
    notifier: NotifyFn,
    /// Serialises creation so the run cap cannot be exceeded by two concurrent
    /// starts.
    open_gate: Mutex<()>,
    next_generation: std::sync::atomic::AtomicU64,
}

impl RuntimeManager {
    pub fn new(
        service: Arc<MultiCliService>,
        data_root: PathBuf,
        profile: String,
        notifier: NotifyFn,
    ) -> Result<Self, AppError> {
        let launch_paths = LaunchPaths::resolve(&data_root)?;
        Ok(Self {
            host_id: "local-windows".into(),
            profile,
            service,
            data_root,
            launch_paths,
            runs: Mutex::new(BTreeMap::new()),
            notifier,
            open_gate: Mutex::new(()),
            next_generation: std::sync::atomic::AtomicU64::new(1),
        })
    }

    pub fn service(&self) -> &Arc<MultiCliService> {
        &self.service
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    fn runtime_log_dir(&self) -> PathBuf {
        self.data_root.join("logs").join("runtime")
    }

    // -----------------------------------------------------------------------
    // Capability table
    // -----------------------------------------------------------------------

    /// What this build can actually do, per product, with the evidence layer
    /// spelled out. Nothing here is promoted by a fixture.
    pub fn capabilities(&self) -> Result<RuntimeCapabilitiesView, AppError> {
        let agents = self.service.agents_by_id()?;
        let installations = self.service.list_all_installations(&self.host_id)?;
        let mut items = Vec::new();
        for product in RuntimeProduct::ALL {
            let matching: Vec<&AgentInstallation> = installations
                .iter()
                .filter(|installation| installation.agent_id == product.agent_id())
                .collect();
            if matching.is_empty() {
                items.push(self.capability_row(
                    product,
                    None,
                    &agents,
                    "unsupported",
                    "未在本机注册该产品的安装；发现页确认后再接入".into(),
                ));
                continue;
            }
            for installation in matching {
                // Readiness is about whether the executable can be launched.
                // It is *not* the capability state and *not* the evidence tier.
                let ready = installation.executable.canonical_path.is_some()
                    && installation_path(&installation.executable).is_file();
                items.push(self.capability_row(
                    product,
                    Some(installation),
                    &agents,
                    if ready { "ready" } else { "unsupported" },
                    if ready {
                        "该安装可启动；能力按版本证据逐字段判定".to_string()
                    } else {
                        "该安装没有可用的 canonical 可执行文件，不能作为结构化目标".to_string()
                    },
                ));
            }
        }
        Ok(RuntimeCapabilitiesView {
            schema_version: RUNTIME_SCHEMA_VERSION,
            items,
            note: format!(
                "{RUNTIME_NOTE} 结构化能力按安装版本逐条标注 supported/unsupported/unverified，证据层另列。"
            ),
        })
    }

    fn capability_row(
        &self,
        product: RuntimeProduct,
        installation: Option<&AgentInstallation>,
        agents: &BTreeMap<String, AgentDefinition>,
        status: &str,
        reason: String,
    ) -> RuntimeCapabilityView {
        let version = installation.and_then(|item| item.version.clone());
        let facts = facts_for(product, version.as_deref());
        let (evidence, note) = evidence_for(product, version.as_deref());
        let installed = installation.is_some();
        // Capability fields are `supported` / `unsupported` / `unverified` only.
        // The evidence tier (`live` / `document` / `fixture`) is a separate
        // field: a live observation of one version never authorises a claim
        // about another version.
        let field = |value: &'static str| -> String {
            if !installed {
                "unsupported".to_string()
            } else if status != "ready" {
                "unsupported".to_string()
            } else {
                value.to_string()
            }
        };
        let executable = installation.map(|item| {
            super::model::host_path(&self.host_id, &installation_path(&item.executable))
        });
        RuntimeCapabilityView {
            product: product.product_key().to_string(),
            product_name: agents
                .get(product.agent_id())
                .map(|agent| agent.display_name.clone())
                .unwrap_or_else(|| product.display_name().to_string()),
            agent_id: product.agent_id().to_string(),
            installation_id: installation.map(|item| item.id.clone()),
            version: installation.and_then(|item| item.version.clone()),
            executable,
            transport: product.transport().to_string(),
            structured_chat: if installed {
                field(facts.chat)
            } else {
                status.to_string()
            },
            permission_flow: if installed {
                field(facts.permission)
            } else {
                status.to_string()
            },
            tool_events: if installed {
                field(facts.tools)
            } else {
                status.to_string()
            },
            cancel: if installed {
                field(facts.cancel)
            } else {
                status.to_string()
            },
            resume: if installed {
                field(facts.resume)
            } else {
                status.to_string()
            },
            evidence: evidence.to_string(),
            entry: installation.map(|item| {
                format!(
                    "{} {}",
                    installation_path(&item.executable).display(),
                    super::launch::product_args(product, "<session-id>", None).join(" ")
                )
            }),
            reason: Some(reason),
            notes: vec![note],
        }
    }

    // -----------------------------------------------------------------------
    // Start
    // -----------------------------------------------------------------------

    pub fn start(&self, request: &RuntimeStartRequest) -> Result<RuntimeRunView, AppError> {
        request.validate()?;
        let _gate = self.open_gate.lock();
        self.evict_finished();
        {
            let runs = self.runs.lock();
            if runs.len() >= MAX_RUNS {
                return Err(AppError::conflict(
                    "runtime",
                    format!("run limit ({MAX_RUNS}) reached; end one first"),
                )
                .with_details(json!({ "active": runs.len(), "limit": MAX_RUNS })));
            }
        }

        let (installation, product) = self.resolve_installation(&request.installation_id)?;
        let executable = installation_path(&installation.executable);
        let chain = LaunchChain::for_extension(&executable).ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported,
                "this executable extension has no implemented launch chain",
            )
            .with_details(json!({
                "stage": "start",
                "extension": executable
                    .extension()
                    .map(|value| value.to_string_lossy().to_string()),
            }))
        })?;

        let run_id = format!("run-{}", Uuid::new_v4().simple());
        let session_id = format!("session-{}", Uuid::new_v4().simple());
        let new_session_id = Uuid::new_v4().to_string();
        let title = request
            .title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "{} · {}",
                    product.display_name(),
                    Path::new(&request.cwd)
                        .file_name()
                        .map(|value| value.to_string_lossy().to_string())
                        .unwrap_or_else(|| request.cwd.clone())
                )
            });
        let cwd = super::model::host_path(&self.host_id, Path::new(&request.cwd));

        let run = Arc::new(RuntimeRun::new(
            Arc::clone(&self.service),
            Arc::clone(&self.notifier),
            run_id.clone(),
            session_id.clone(),
            product.agent_id().to_string(),
            Some(installation.id.clone()),
            product,
            title.clone(),
            cwd.clone(),
            request.permission_timeout(),
            self.next_generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ));

        // The Session and the Run exist before anything is spawned: a failure
        // below leaves a real `failed` Run that the UI can explain.
        self.begin_records(&run, &cwd, product)?;
        if let Err(error) = run.begin() {
            return Err(error);
        }

        let planned = match plan(
            LaunchRequest {
                product,
                executable,
                chain,
                cwd: cwd_path(&cwd),
                run_id: &run_id,
                new_session_id: &new_session_id,
                resume_session: None,
            },
            &self.launch_paths,
        ) {
            Ok(planned) => planned,
            Err(error) => {
                run.fail(&error);
                return Err(error);
            }
        };
        run.set_launch_argv(planned.plan.argv_preview.clone());
        self.spawn_and_connect(
            &run,
            planned,
            product,
            &cwd,
            ClaudeResume::New(&new_session_id),
            request.model.as_deref(),
            request.effort.as_deref(),
            request.prompt.as_deref(),
        )?;
        self.runs.lock().insert(run_id.clone(), Arc::clone(&run));
        Ok(run.view())
    }

    /// Continues a historical Session: a new Run, same Session, same native id.
    pub fn resume(&self, request: &RuntimeResumeRequest) -> Result<RuntimeRunView, AppError> {
        request.validate()?;
        let _gate = self.open_gate.lock();
        self.evict_finished();
        {
            let runs = self.runs.lock();
            if runs.len() >= MAX_RUNS {
                return Err(AppError::conflict(
                    "runtime",
                    format!("run limit ({MAX_RUNS}) reached; end one first"),
                )
                .with_details(json!({ "active": runs.len(), "limit": MAX_RUNS })));
            }
        }
        let session = self.session_row(&request.session_id)?;
        let Some(product) = RuntimeProduct::for_agent(&session.agent_id) else {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "this session belongs to a product the structured runtime does not implement",
            )
            .with_details(json!({ "sessionId": session.id, "agentId": session.agent_id })));
        };
        if session.native_id.is_none() {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "this session has no native id, so no conversation can be continued",
            )
            .with_details(json!({ "sessionId": session.id })));
        }
        // Conflict resolution is S03's, reused rather than re-guessed: a live
        // owner of the same native session blocks, an unverifiable one warns.
        let target = resolve_resume_target(&self.service, &session.id)?;
        if target.status == "blocked" {
            return Err(AppError::conflict(
                "session",
                "this session still has an active run; continuing would double-write it",
            )
            .with_details(json!({
                "sessionId": session.id,
                "conflicts": target.conflicts,
            })));
        }
        if let Some(existing) = self.active_run_for_session(&session.id) {
            return Err(AppError::conflict(
                "session",
                "this session already has a live managed run in this application",
            )
            .with_details(json!({ "sessionId": session.id, "runId": existing })));
        }

        let installation = match &request.installation_id {
            Some(id) => self.resolve_installation(id)?.0,
            None => {
                let id = target.installation_id.clone().ok_or_else(|| {
                    AppError::new(
                        ErrorCode::Unverified,
                        "no confirmed installation is registered for this session",
                    )
                    .with_details(json!({ "sessionId": session.id }))
                })?;
                self.resolve_installation(&id)?.0
            }
        };
        if installation.agent_id != session.agent_id {
            return Err(AppError::invalid_argument(
                "installationId",
                "the installation belongs to a different product than the session",
            ));
        }
        let executable = installation_path(&installation.executable);
        let chain = LaunchChain::for_extension(&executable).ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported,
                "this executable extension has no implemented launch chain",
            )
        })?;
        let cwd_text = match &request.cwd {
            Some(cwd) => cwd.clone(),
            None => target
                .workspaces
                .first()
                .and_then(|path| path.canonical_path.clone())
                .or_else(|| {
                    target
                        .workspaces
                        .first()
                        .map(|path| path.display_path.clone())
                })
                .ok_or_else(|| {
                    AppError::invalid_argument(
                        "cwd",
                        "this session has no recorded workspace; provide an absolute directory",
                    )
                    .with_details(json!({ "sessionId": session.id }))
                })?,
        };
        if !Path::new(&cwd_text).is_dir() {
            return Err(AppError::invalid_argument(
                "cwd",
                "the recorded workspace no longer exists; provide an absolute directory",
            )
            .with_details(json!({ "sessionId": session.id, "cwd": cwd_text })));
        }
        let cwd = super::model::host_path(&self.host_id, Path::new(&cwd_text));
        let run_id = format!("run-{}", Uuid::new_v4().simple());
        let title = format!("{} · 继续", session.title);
        let run = Arc::new(RuntimeRun::new(
            Arc::clone(&self.service),
            Arc::clone(&self.notifier),
            run_id.clone(),
            session.id.clone(),
            product.agent_id().to_string(),
            Some(installation.id.clone()),
            product,
            title,
            cwd.clone(),
            request
                .permission_timeout_ms
                .map(Duration::from_millis)
                .unwrap_or_else(|| {
                    Duration::from_millis(super::model::DEFAULT_PERMISSION_TIMEOUT_MS)
                }),
            self.next_generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ));
        if target.status == "unknown" {
            run.note(format!(
                "继续前的冲突检查结果为 unknown：{}（未自动停止任何外部会话）",
                target.reason
            ));
        }
        if let Err(error) = run.begin() {
            return Err(error);
        }
        let planned = match plan(
            LaunchRequest {
                product,
                executable,
                chain,
                cwd: cwd_path(&cwd),
                run_id: &run_id,
                new_session_id: "",
                resume_session: session.native_id.as_deref(),
            },
            &self.launch_paths,
        ) {
            Ok(planned) => planned,
            Err(error) => {
                run.fail(&error);
                return Err(error);
            }
        };
        run.set_launch_argv(planned.plan.argv_preview.clone());
        self.spawn_and_connect(
            &run,
            planned,
            product,
            &cwd,
            ClaudeResume::Existing(session.native_id.clone().unwrap_or_default()),
            None,
            None,
            request.prompt.as_deref(),
        )?;
        self.runs.lock().insert(run_id, Arc::clone(&run));
        Ok(run.view())
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_and_connect(
        &self,
        run: &Arc<RuntimeRun>,
        planned: super::launch::PlannedLaunch,
        product: RuntimeProduct,
        cwd: &HostPath,
        resume: ClaudeResume<'_>,
        model: Option<&str>,
        effort: Option<&str>,
        prompt: Option<&str>,
    ) -> Result<(), AppError> {
        // The product delta first, then the launcher's own values: a `.cmd`
        // target is started through a trampoline whose path arrives as
        // `MULTICLI_CMD_FILE`, and without it cmd expands an empty variable and
        // exits before the protocol even starts. Product values never override
        // the launcher ones, which are the only thing keeping the argv safe.
        let mut env = planned.env.clone();
        env.extend(planned.plan.launcher_env.iter().cloned());
        let spec = ProcessSpec {
            program: planned.plan.program.clone(),
            args: planned.plan.program_args.clone(),
            cwd: cwd_path(cwd),
            env,
            env_remove: planned.env_remove.clone(),
        };
        let (process, stdout) = match OwnedProcess::spawn(&self.host_id, &spec) {
            Ok(value) => value,
            Err(error) => {
                run.fail(&error);
                let _ = crate::multicli::terminal::launch::cleanup_trampoline(
                    &self.launch_paths,
                    &run.run_id,
                );
                return Err(error);
            }
        };
        let process = Arc::new(process);
        let emitter = Emitter::new(run.sink());
        let adapter_result: Result<Arc<dyn super::run::Adapter>, AppError> = match product {
            RuntimeProduct::Codex => codex::CodexAdapter::connect(
                Arc::clone(&process),
                stdout,
                emitter,
                codex::CodexOptions {
                    cwd: cwd_path(cwd).display().to_string(),
                    // Least privilege: every write and every command that
                    // would leave the sandbox becomes a real approval request.
                    sandbox: "read-only".into(),
                    model: model.map(str::to_string),
                    effort: effort.map(str::to_string),
                    resume_thread: match &resume {
                        ClaudeResume::Existing(id) => Some(id.clone()),
                        ClaudeResume::New(_) => None,
                    },
                },
            )
            .map(|adapter| adapter as Arc<dyn super::run::Adapter>),
            RuntimeProduct::ClaudeCode => {
                let new_session_id = match &resume {
                    ClaudeResume::New(id) => (*id).to_string(),
                    ClaudeResume::Existing(id) => id.clone(),
                };
                claude::ClaudeAdapter::connect(
                    Arc::clone(&process),
                    stdout,
                    emitter,
                    claude::ClaudeOptions {
                        resume_session: match &resume {
                            ClaudeResume::Existing(id) => Some(id.clone()),
                            ClaudeResume::New(_) => None,
                        },
                        new_session_id,
                    },
                )
                .map(|adapter| adapter as Arc<dyn super::run::Adapter>)
            }
            RuntimeProduct::CodeBuddy => {
                let resume_session = match &resume {
                    ClaudeResume::Existing(id) => Some(id.clone()),
                    ClaudeResume::New(_) => None,
                };
                codebuddy::CodeBuddyAdapter::connect(
                    Arc::clone(&process),
                    stdout,
                    emitter,
                    codebuddy::CodeBuddyOptions {
                        cwd: cwd_path(cwd).display().to_string(),
                        resume_session,
                        load_only: false,
                    },
                )
                .map(|adapter| adapter as Arc<dyn super::run::Adapter>)
            }
        };
        let adapter = match adapter_result {
            Ok(adapter) => adapter,
            Err(error) => {
                process.kill_tree();
                let tail = process.stderr_tail();
                if !tail.is_empty() {
                    // A protocol failure is almost always explained on stderr
                    // (an EADDRINUSE from an inherited port, a missing entry
                    // script, a usage error). It is redacted and bounded, so it
                    // is surfaced rather than swallowed into a note nobody can
                    // see when `start` itself failed.
                    eprintln!("[multicli] CLI stderr: {}", tail.join(" | "));
                    run.note(format!("CLI stderr: {}", tail.join(" | ")));
                }
                run.fail(&error);
                let _ = crate::multicli::terminal::launch::cleanup_trampoline(
                    &self.launch_paths,
                    &run.run_id,
                );
                return Err(error);
            }
        };
        run.attach_adapter(adapter, Arc::clone(&process));
        if let Some(native_id) = run_native_id(run) {
            if let Err(error) = self
                .service
                .set_session_native_id(&run.session_id, &native_id)
            {
                run.note(format!("native id 未能写回 Session：{}", error.message));
            }
        }
        if let Some(prompt) = prompt {
            if let Err(error) = run.send(prompt) {
                run.note(format!("首条消息未能送达：{}", error.message));
                // This Run has not yet entered the manager's map, so a failed
                // start/resume must close its owned process here.
                run.stop("首条消息发送失败", Duration::ZERO);
                let _ = crate::multicli::terminal::launch::cleanup_trampoline(
                    &self.launch_paths,
                    &run.run_id,
                );
                return Err(error);
            }
        }
        Ok(())
    }

    fn begin_records(
        &self,
        run: &Arc<RuntimeRun>,
        cwd: &HostPath,
        product: RuntimeProduct,
    ) -> Result<(), AppError> {
        self.service.ensure_session(&Session {
            id: run.session_id.clone(),
            agent_id: product.agent_id().to_string(),
            host_id: self.host_id.clone(),
            source_namespace: format!("{}:runtime:{}", product.agent_id(), self.host_id),
            native_id: None,
            origin: SessionOrigin::Managed,
            title: run.title.clone(),
        })?;
        let workspace = self
            .service
            .ensure_workspace_at_path(&self.host_id, &cwd.display_path)?;
        self.service.attach_workspace(&SessionWorkspace {
            session_id: run.session_id.clone(),
            workspace_id: workspace.id,
            role: SessionWorkspaceRole::Primary,
            evidence: "multicli-runtime-v1: explicit managed working directory".into(),
        })?;
        Ok(())
    }

    fn resolve_installation(
        &self,
        installation_id: &str,
    ) -> Result<(AgentInstallation, RuntimeProduct), AppError> {
        let installation = self
            .service
            .get_installation(&self.host_id, installation_id)?
            .ok_or_else(|| {
                AppError::not_found("installation", installation_id)
                    .with_details(json!({ "hostId": self.host_id }))
            })?;
        let product = RuntimeProduct::for_agent(&installation.agent_id).ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported,
                "this installation is not one of the three supported CLI products",
            )
            .with_details(json!({ "agentId": installation.agent_id }))
        })?;
        if installation.executable.canonical_path.is_none() {
            return Err(AppError::new(
                ErrorCode::Unverified,
                "this installation has no canonical executable identity",
            )
            .with_details(json!({ "installationId": installation.id })));
        }
        if !installation_path(&installation.executable).is_file() {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "the registered executable no longer exists",
            )
            .with_details(json!({ "installationId": installation.id })));
        }
        Ok((installation, product))
    }

    // -----------------------------------------------------------------------
    // Operations
    // -----------------------------------------------------------------------

    fn run(&self, run_id: &str) -> Result<Arc<RuntimeRun>, AppError> {
        self.runs
            .lock()
            .get(run_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("run", run_id))
    }

    pub fn send(&self, request: &RuntimeSendRequest) -> Result<RuntimeRunView, AppError> {
        request.validate()?;
        self.run(&request.run_id)?.send(&request.text)
    }

    pub fn cancel(&self, request: &RuntimeCancelRequest) -> Result<RuntimeRunView, AppError> {
        request.validate()?;
        self.run(&request.run_id)?
            .cancel("用户在 MultiCLI 中取消", request.grace())
    }

    pub fn answer(&self, request: &RuntimeAnswerRequest) -> Result<RuntimeRunView, AppError> {
        request.validate()?;
        self.run(&request.run_id)?
            .answer(&request.permission_id, &request.option_id)
    }

    pub fn stop(&self, request: &RuntimeStopRequest) -> Result<RuntimeRunView, AppError> {
        request.validate()?;
        let run = self.run(&request.run_id)?;
        let view = run.stop("用户结束了这次运行", request.grace());
        let _ =
            crate::multicli::terminal::launch::cleanup_trampoline(&self.launch_paths, &run.run_id);
        Ok(view)
    }

    pub fn get(&self, run_id: &str) -> Result<RuntimeRunView, AppError> {
        Ok(self.run(run_id)?.view())
    }

    pub fn read(&self, request: &RuntimeReadRequest) -> Result<RuntimeEventPage, AppError> {
        request.validate()?;
        Ok(self
            .run(&request.run_id)?
            .read(request.cursor, request.limit()))
    }

    pub fn list(&self) -> RuntimeListView {
        let runs: Vec<Arc<RuntimeRun>> = self.runs.lock().values().cloned().collect();
        let mut items: Vec<RuntimeRunView> = runs.iter().map(|run| run.view()).collect();
        items.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.run_id.cmp(&right.run_id))
        });
        let active_count = items
            .iter()
            .filter(|item| !matches!(item.execution_state.as_str(), "exited" | "failed"))
            .count() as u32;
        RuntimeListView {
            schema_version: RUNTIME_SCHEMA_VERSION,
            items,
            active_count,
            max_runs: MAX_RUNS as u32,
            note: RUNTIME_NOTE.to_string(),
        }
    }

    /// Ends every owned connection. Called from the application exit event:
    /// external CLI processes are not enumerated at all.
    pub fn shutdown_all(&self) {
        let runs: Vec<Arc<RuntimeRun>> = {
            let mut map = self.runs.lock();
            std::mem::take(&mut *map).into_values().collect()
        };
        for run in runs {
            let _ = run.stop("应用退出", Duration::from_millis(1_500));
            let _ = crate::multicli::terminal::launch::cleanup_trampoline(
                &self.launch_paths,
                &run.run_id,
            );
        }
    }

    /// Expires unanswered permission requests. Safe to call from a timer: it
    /// never blocks on a protocol write for long, and it only answers with the
    /// request's own deny option.
    pub fn sweep_permissions(&self) {
        let runs: Vec<Arc<RuntimeRun>> = self.runs.lock().values().cloned().collect();
        for run in runs {
            if run.has_pending_work() {
                run.sweep_permissions();
            }
        }
    }

    fn evict_finished(&self) {
        let mut runs = self.runs.lock();
        let finished: Vec<String> = runs
            .iter()
            .filter(|(_, run)| run.is_terminal())
            .map(|(id, _)| id.clone())
            .collect();
        for id in finished {
            runs.remove(&id);
        }
    }

    fn active_run_for_session(&self, session_id: &str) -> Option<String> {
        self.runs
            .lock()
            .values()
            .find(|run| run.session_id == session_id && !run.is_terminal())
            .map(|run| run.run_id.clone())
    }

    fn session_row(&self, session_id: &str) -> Result<Session, AppError> {
        let detail = self.service.get_session(session_id)?;
        Ok(detail.session)
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// Recorded for the delivery report: the run directory holds the per-run
    /// trampolines and nothing else.
    pub fn run_dir(&self) -> PathBuf {
        self.runtime_log_dir()
    }
}

enum ClaudeResume<'a> {
    New(&'a str),
    Existing(String),
}

fn run_native_id(run: &Arc<RuntimeRun>) -> Option<String> {
    let view = run.view();
    view.native_session_id
}

/// The exact CLI version this stage measured, per product.
fn verified_version(product: RuntimeProduct) -> &'static str {
    match product {
        RuntimeProduct::Codex => "0.166.0",
        RuntimeProduct::ClaudeCode => "2.1.153",
        RuntimeProduct::CodeBuddy => "2.179.3",
    }
}

/// True only for the version this stage actually observed. A different or
/// unknown version inherits nothing: its capabilities stay `unverified` until
/// someone measures them.
fn version_is_measured(product: RuntimeProduct, version: Option<&str>) -> bool {
    match version {
        Some(value) => {
            let trimmed = value.trim();
            trimmed.split_whitespace().next() == Some(verified_version(product))
        }
        None => false,
    }
}

/// Per-product evidence for the measured version only.
fn evidence_for(product: RuntimeProduct, version: Option<&str>) -> (&'static str, String) {
    if !version_is_measured(product, version) {
        return (
            "document",
            "该安装版本没有本阶段的实测证据：能力字段一律 unverified，不继承已验证版本的能力"
                .to_string(),
        );
    }
    match product {
        RuntimeProduct::Codex => (
            "live",
            "app-server 0.166.0：正文、工具、拒绝后文件未生成、允许后文件生成、对端确认取消、thread/resume 同一会话新Run并收到新回复均已实测。仅本次有限样本，要求模型端点可达；历史失败不代表当前不支持。".to_string(),
        ),
        RuntimeProduct::ClaudeCode => (
            "live",
            "2.1.153 的 stream-json 回合、control_request 权限往返（拒绝→文件不存在 / 允许→文件生成）、interrupt 取消与 --resume 继续均已实测通过。".to_string(),
        ),
        RuntimeProduct::CodeBuddy => (
            "live",
            "ACP 2.179.3：正文、文件工具、Reject后文件不存在、Allow后文件生成、取消、session/load后同一会话新Run并收到新回复均已观察到。Reject会结束本轮；未调用authenticate或提取凭据。旧认证失败保留，恢复原因不作推断，仅本次有限样本。".to_string(),
        ),
    }
}

/// One row of measured capability per product **and version**. A field is
/// `supported` only when this stage reproduced it on that exact version.
struct ProductFacts {
    chat: &'static str,
    permission: &'static str,
    tools: &'static str,
    cancel: &'static str,
    resume: &'static str,
}

fn facts_for(product: RuntimeProduct, version: Option<&str>) -> ProductFacts {
    if !version_is_measured(product, version) {
        return ProductFacts {
            chat: "unverified",
            permission: "unverified",
            tools: "unverified",
            cancel: "unverified",
            resume: "unverified",
        };
    }
    match product {
        RuntimeProduct::Codex => ProductFacts {
            // Measured end to end once the endpoint was reachable: assistant
            // text, a commandExecution tool item, and both approval outcomes
            // (deny left no file, allow created one).
            chat: "supported",
            permission: "supported",
            tools: "supported",
            // `turn/interrupt` is confirmed by the peer: the turn reaches
            // `cancelled` with an end timestamp.
            cancel: "supported",
            resume: "supported",
        },
        RuntimeProduct::ClaudeCode => ProductFacts {
            chat: "supported",
            permission: "supported",
            tools: "supported",
            cancel: "supported",
            resume: "supported",
        },
        RuntimeProduct::CodeBuddy => ProductFacts {
            chat: "supported",
            permission: "supported",
            tools: "supported",
            cancel: "supported",
            resume: "supported",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_table_never_promotes_an_unverified_flow() {
        let claude = facts_for(RuntimeProduct::ClaudeCode, Some("2.1.153"));
        assert_eq!(claude.permission, "supported");
        assert_eq!(claude.chat, "supported");

        // Codex was measured end to end once its endpoint was reachable:
        // text, tool items, both approval outcomes, and a confirmed cancel.
        let codex = facts_for(RuntimeProduct::Codex, Some("0.166.0"));
        assert_eq!(codex.cancel, "supported");
        assert_eq!(codex.chat, "supported");
        assert_eq!(codex.permission, "supported");
        assert_eq!(codex.resume, "supported");

        let codebuddy = facts_for(RuntimeProduct::CodeBuddy, Some("2.179.3"));
        assert_eq!(codebuddy.chat, "supported");
        assert_eq!(codebuddy.permission, "supported");
        assert_eq!(codebuddy.resume, "supported");
    }

    #[test]
    fn an_unknown_version_inherits_nothing() {
        for product in RuntimeProduct::ALL {
            let prefix_collision = format!("{}1", verified_version(product));
            assert!(!version_is_measured(product, Some(&prefix_collision)));
            for version in [None, Some("9999.0"), Some("")] {
                let facts = facts_for(product, version);
                for field in [
                    facts.chat,
                    facts.permission,
                    facts.tools,
                    facts.cancel,
                    facts.resume,
                ] {
                    assert_eq!(field, "unverified", "{product:?} {version:?}");
                }
                assert_eq!(evidence_for(product, version).0, "document");
            }
        }
        // A measured version keeps its live evidence.
        assert_eq!(
            evidence_for(RuntimeProduct::ClaudeCode, Some("2.1.153")).0,
            "live"
        );
        // A suffix is tolerated: discovery records the raw `--version` output.
        assert!(version_is_measured(
            RuntimeProduct::ClaudeCode,
            Some("2.1.153 (Claude Code)")
        ));
    }

    #[test]
    fn every_product_reports_an_evidence_layer_for_its_measured_version() {
        for product in RuntimeProduct::ALL {
            let measured = evidence_for(product, Some(verified_version(product)));
            assert_eq!(measured.0, "live");
            assert!(!measured.1.is_empty());

            // The same product at an unmeasured version is documentation only,
            // and says so.
            let other = evidence_for(product, Some("0.0.0-synthetic"));
            assert_eq!(other.0, "document");
            assert!(other.1.contains("unverified"));
        }
    }
}
