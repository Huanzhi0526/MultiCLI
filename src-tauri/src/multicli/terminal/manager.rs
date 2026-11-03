//! Managed terminal registry and lifecycle orchestration.
//!
//! The manager is the only place that creates a terminal `Run`: it resolves the
//! target, records the Run as `starting`, spawns the PTY, proves ownership of
//! the real process identity, links the process to the Run and only then marks
//! it `running`. Any failure before that point ends the Run as `failed` and
//! removes what was created, so a failed open can never leave a successful Run
//! or an orphan tree behind.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::multicli::domain::{
    AgentDefinition, AgentInstallation, EvidenceLevel, ExecutionStatus, HostPath,
    InteractionStatus, LinkKind, ProcessIdentity, ProcessObservation, Run, RunOwnership,
    RunProcessLink, RunTransport, Session, SessionOrigin, SessionWorkspace, SessionWorkspaceRole,
};
use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::history::ids::iso_from_millis;
use crate::multicli::MultiCliService;

use super::launch::{
    cleanup_trampoline, plan_launch, shell_candidates, shell_path, LaunchPaths, PinnedTarget,
};
use super::logfile::TerminalLog;
use super::model::{
    host_path, EnvVar, LaunchChain, TerminalCloseRequest, TerminalCloseView, TerminalLimits,
    TerminalLimitsUpdate, TerminalListView, TerminalOpenSpec, TerminalQuitCheckView,
    TerminalReadRequest, TerminalReadView, TerminalResizeRequest, TerminalResizeView,
    TerminalShutdownAllRequest, TerminalShutdownAllView, TerminalState, TerminalStopMode,
    TerminalStopReport, TerminalStopRequest, TerminalStopView, TerminalTargetKind,
    TerminalTargetView, TerminalTargetsView, TerminalView, TerminalWriteRequest,
    TerminalWriteView, DEFAULT_LOG_BYTES, MAX_TERMINALS, OWNERSHIP_NOTE, STREAM_NOTE,
    TERMINAL_SCHEMA_VERSION,
};
use super::session::{chain_note, NotifyFn, SessionConfig, TerminalFinish, TerminalSession};

/// Built-in definition for the plain-shell target. It is *not* a CLI product:
/// no discovery, history or protocol path ever treats it as one, and it exists
/// only so a managed PTY Run has an honest session owner.
pub const LOCAL_SHELL_AGENT_ID: &str = "local-shell";

fn now_iso() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0);
    iso_from_millis(millis)
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LimitsFile {
    buffer_bytes: Option<u64>,
    log_bytes: Option<u64>,
}

pub struct TerminalManager {
    host_id: String,
    profile: String,
    service: Arc<MultiCliService>,
    data_root: PathBuf,
    limits: Mutex<TerminalLimits>,
    settings_note: Mutex<Option<String>>,
    sessions: Mutex<BTreeMap<String, Arc<TerminalSession>>>,
    launch_paths: LaunchPaths,
    notifier: NotifyFn,
    /// Serialises creation so the terminal cap cannot be exceeded by two
    /// concurrent opens.
    open_gate: Mutex<()>,
}

impl TerminalManager {
    pub fn new(
        service: Arc<MultiCliService>,
        data_root: PathBuf,
        profile: String,
        notifier: NotifyFn,
    ) -> Result<Self, AppError> {
        let launch_paths = LaunchPaths::resolve(&data_root)?;
        let (limits, note) = load_limits(&data_root);
        let manager = Self {
            host_id: "local-windows".into(),
            profile,
            service,
            data_root,
            limits: Mutex::new(limits),
            settings_note: Mutex::new(note),
            sessions: Mutex::new(BTreeMap::new()),
            launch_paths,
            notifier,
            open_gate: Mutex::new(()),
        };
        manager.ensure_local_shell_agent()?;
        Ok(manager)
    }

    pub fn service(&self) -> &Arc<MultiCliService> {
        &self.service
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    fn ensure_local_shell_agent(&self) -> Result<(), AppError> {
        self.service.register_agent(&AgentDefinition {
            id: LOCAL_SHELL_AGENT_ID.into(),
            vendor: "local".into(),
            display_name: "本地 Shell".into(),
        })?;
        Ok(())
    }

    fn log_dir(&self) -> PathBuf {
        self.data_root.join("logs").join("terminal")
    }

    pub fn limits(&self) -> TerminalLimits {
        self.limits.lock().clone()
    }

    pub fn update_limits(&self, update: &TerminalLimitsUpdate) -> Result<TerminalLimits, AppError> {
        update.validate()?;
        let mut limits = self.limits.lock();
        let mut next = limits.clone();
        update.apply(&mut next);
        let payload = LimitsFile {
            buffer_bytes: Some(next.buffer_bytes),
            log_bytes: Some(next.log_bytes),
        };
        let path = self.data_root.join("terminal-settings.json");
        let text = serde_json::to_string_pretty(&payload).map_err(|error| {
            AppError::new(ErrorCode::Protocol, format!("encode terminal settings: {error}"))
        })?;
        std::fs::write(&path, text).map_err(|error| {
            AppError::new(
                ErrorCode::Io,
                format!("failed to persist terminal settings: {error}"),
            )
            .with_details(serde_json::json!({ "stage": "updateLimits" }))
            .with_retryable(true)
        })?;
        *limits = next.clone();
        Ok(next)
    }

    // -----------------------------------------------------------------------
    // Targets
    // -----------------------------------------------------------------------

    pub fn targets(&self) -> Result<TerminalTargetsView, AppError> {
        let mut items = Vec::new();
        for shell in shell_candidates() {
            items.push(TerminalTargetView {
                id: format!("shell:{}", shell.flavor.as_str()),
                kind: TerminalTargetKind::Shell,
                label: shell.label,
                agent_id: Some(LOCAL_SHELL_AGENT_ID.to_string()),
                product: None,
                installation_id: None,
                executable: Some(host_path(&self.host_id, &shell.path)),
                launch_chain: Some(LaunchChain::Exe),
                status: if shell.exists { "ready" } else { "missing" }.into(),
                reason: if shell.exists {
                    None
                } else {
                    Some("该 shell 在本机不存在".into())
                },
                version: None,
            });
        }
        let agents = self.service.agents_by_id()?;
        let installations = self.service.list_all_installations(&self.host_id)?;
        for installation in installations {
            items.push(self.installation_target(&installation, &agents));
        }
        Ok(TerminalTargetsView {
            schema_version: TERMINAL_SCHEMA_VERSION,
            items,
            limits: self.limits(),
            cwd_default: dirs::home_dir()
                .filter(|path| path.is_dir())
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            note: format!("{STREAM_NOTE} 目标只来自本机已注册安装与已知 shell，创建时固定可执行文件身份。"),
        })
    }

    fn installation_target(
        &self,
        installation: &AgentInstallation,
        agents: &BTreeMap<String, AgentDefinition>,
    ) -> TerminalTargetView {
        let path = PathBuf::from(
            installation
                .executable
                .canonical_path
                .clone()
                .unwrap_or_else(|| installation.executable.display_path.clone()),
        );
        let exists = path.is_file();
        let chain = LaunchChain::for_extension(&path);
        let product = agents
            .get(&installation.agent_id)
            .map(|agent| agent.display_name.clone());
        let file_name = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let (status, reason) = match (exists, chain, &installation.executable.canonical_path) {
            (false, _, _) => (
                "missing",
                Some("可执行文件不存在；不会启动一个已失效的安装".to_string()),
            ),
            (true, None, _) => (
                "unsupported",
                Some("扩展名不属于 .exe/.cmd/.bat/.ps1，未实现该启动链".to_string()),
            ),
            (true, Some(_), None) => (
                "unverified",
                Some("该安装只有显示路径，没有 canonical 身份；不视为已核实的目标".to_string()),
            ),
            (true, Some(LaunchChain::Powershell), Some(_))
                if shell_path(super::model::ShellFlavor::Pwsh).is_err() => (
                    "unsupported",
                    Some(".ps1 的已验证启动链需要 PowerShell 7.3+ (pwsh)；可使用该 CLI 的 .exe/.cmd 入口".to_string()),
                ),
            (true, Some(_), Some(_)) => ("ready", None),
        };
        TerminalTargetView {
            id: format!("installation:{}", installation.id),
            kind: TerminalTargetKind::Installation,
            label: format!(
                "{} · {}",
                product.clone().unwrap_or_else(|| installation.agent_id.clone()),
                file_name
            ),
            agent_id: Some(installation.agent_id.clone()),
            product,
            installation_id: Some(installation.id.clone()),
            executable: Some(host_path(&self.host_id, &path)),
            launch_chain: chain,
            status: status.into(),
            reason,
            version: installation.version.clone(),
        }
    }

    fn resolve_target(&self, spec: &TerminalOpenSpec) -> Result<PinnedTarget, AppError> {
        match spec.target.kind {
            TerminalTargetKind::Shell => {
                let flavor = spec.target.shell.unwrap_or(super::model::ShellFlavor::Cmd);
                let path = shell_path(flavor)?;
                Ok(PinnedTarget {
                    kind: TerminalTargetKind::Shell,
                    label: path
                        .file_name()
                        .map(|value| value.to_string_lossy().to_string())
                        .unwrap_or_else(|| "shell".into()),
                    product: None,
                    agent_id: LOCAL_SHELL_AGENT_ID.into(),
                    installation_id: None,
                    executable: path,
                    chain: LaunchChain::Exe,
                    shell_flavor: Some(flavor),
                    version: None,
                })
            }
            TerminalTargetKind::Installation => {
                let installation_id = spec
                    .target
                    .installation_id
                    .clone()
                    .ok_or_else(|| {
                        AppError::invalid_argument(
                            "target.installationId",
                            "is required for an installation target",
                        )
                    })?;
                let installation = self
                    .service
                    .get_installation(&self.host_id, &installation_id)?
                    .ok_or_else(|| {
                        AppError::new(
                            ErrorCode::NotFound,
                            "the selected installation is not registered for this host",
                        )
                        .with_details(serde_json::json!({
                            "entity": "installation",
                            "id": installation_id,
                        }))
                    })?;
                let path = PathBuf::from(
                    installation
                        .executable
                        .canonical_path
                        .clone()
                        .unwrap_or_else(|| installation.executable.display_path.clone()),
                );
                if !path.is_file() {
                    return Err(AppError::new(
                        ErrorCode::Unsupported,
                        "the selected installation has no readable executable",
                    )
                    .with_details(serde_json::json!({
                        "stage": "resolveTarget",
                        "installationId": installation.id,
                    })));
                }
                if installation.executable.canonical_path.is_none() {
                    // Only a canonical identity counts as verified: a display
                    // path alone can be a shim that resolves somewhere else.
                    return Err(AppError::new(
                        ErrorCode::Unverified,
                        "this installation has no canonical executable identity",
                    )
                    .with_details(serde_json::json!({
                        "stage": "resolveTarget",
                        "installationId": installation.id,
                    })));
                }
                let chain = LaunchChain::for_extension(&path).ok_or_else(|| {
                    AppError::new(
                        ErrorCode::Unsupported,
                        "this executable extension has no implemented launch chain",
                    )
                    .with_details(serde_json::json!({
                        "stage": "resolveTarget",
                        "extension": path
                            .extension()
                            .map(|value| value.to_string_lossy().to_string()),
                    }))
                })?;
                let product = self
                    .service
                    .agents_by_id()?
                    .get(&installation.agent_id)
                    .map(|agent| agent.display_name.clone());
                Ok(PinnedTarget {
                    kind: TerminalTargetKind::Installation,
                    label: path
                        .file_name()
                        .map(|value| value.to_string_lossy().to_string())
                        .unwrap_or_else(|| "target".into()),
                    product,
                    agent_id: installation.agent_id,
                    installation_id: Some(installation.id),
                    executable: path,
                    chain,
                    shell_flavor: None,
                    version: installation.version,
                })
            }
        }
    }

    // -----------------------------------------------------------------------
    // Open
    // -----------------------------------------------------------------------

    pub fn open(&self, spec: &TerminalOpenSpec) -> Result<TerminalView, AppError> {
        spec.validate()?;
        let _gate = self.open_gate.lock();
        // Ended terminals still hold a ring, a log handle and a map slot. They
        // are reclaimed only when the cap would otherwise be hit, and the oldest
        // ones first: a finished tab the user is still reading must not vanish
        // just because another terminal was opened.
        let reclaim: Vec<Arc<TerminalSession>> = {
            let mut sessions = self.sessions.lock();
            let mut ended: Vec<(String, String)> = sessions
                .iter()
                .filter(|(_, session)| session.state().is_terminal())
                .map(|(id, session)| (session.started_at.clone(), id.clone()))
                .collect();
            let overflow = (sessions.len() + 1).saturating_sub(MAX_TERMINALS);
            if overflow == 0 || ended.is_empty() {
                Vec::new()
            } else {
                ended.sort();
                ended
                    .into_iter()
                    .take(overflow)
                    .filter_map(|(_, id)| sessions.remove(&id))
                    .collect()
            }
        };
        for session in reclaim {
            session.release();
        }
        {
            let sessions = self.sessions.lock();
            if sessions.len() >= MAX_TERMINALS {
                return Err(AppError::conflict(
                    "terminal",
                    format!("terminal limit ({MAX_TERMINALS}) reached; close one first"),
                )
                .with_details(serde_json::json!({
                    "stage": "open",
                    "active": sessions.len(),
                    "limit": MAX_TERMINALS,
                })));
            }
        }
        let target = self.resolve_target(spec)?;
        let terminal_id = format!("term-{}", Uuid::new_v4().simple());
        let run_id = format!("run-{}", Uuid::new_v4().simple());
        let session_id = format!("session-{}", Uuid::new_v4().simple());
        let started_at = now_iso();
        let title = spec
            .title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "{} · {}",
                    target.label,
                    Path::new(&spec.cwd)
                        .file_name()
                        .map(|value| value.to_string_lossy().to_string())
                        .unwrap_or_else(|| spec.cwd.clone())
                )
            });
        let cwd = host_path(&self.host_id, Path::new(&spec.cwd));

        self.begin_run(&run_id, &session_id, &terminal_id, &target, &title, &started_at, &spec.cwd)?;

        let plan = match plan_launch(&target, &spec.args, &terminal_id, &self.launch_paths) {
            Ok(plan) => plan,
            Err(error) => {
                self.fail_run(&run_id, &error);
                return Err(error);
            }
        };
        let limits = self.limits();
        // The log records *what* was started, never the argument values: an
        // argument can carry a token or a prompt, and a log file outlives the
        // process that held it in memory.
        let header = format!(
            "# MultiCLI managed terminal\r\n# terminalId={terminal_id}\r\n# runId={run_id}\r\n# profile={}\r\n# executable={}\r\n# executableFingerprint={}\r\n# chain={}\r\n# cwd={}\r\n# args={} (values not logged)\r\n# env={:?}\r\n# startedAt={started_at}\r\n# {}\r\n",
            self.profile,
            target.executable.display(),
            plan.fingerprint,
            target.chain.as_str(),
            cwd.display_path,
            spec.args.len(),
            spec.env.iter().map(|value| value.name.clone()).collect::<Vec<_>>(),
            chain_note(target.chain)
        );
        let log = match TerminalLog::create(&self.log_dir(), &terminal_id, limits.log_bytes, &header)
        {
            Ok(log) => log,
            Err(error) => {
                let _ = cleanup_trampoline(&self.launch_paths, &terminal_id);
                self.fail_run(&run_id, &error);
                return Err(error);
            }
        };

        let service = Arc::clone(&self.service);
        let finish: super::session::FinishFn = Arc::new(move |event: TerminalFinish| {
            let status = match event.state {
                TerminalState::Exited => ExecutionStatus::Exited,
                TerminalState::Failed => ExecutionStatus::Failed,
                TerminalState::Stopping => ExecutionStatus::Stopping,
                _ => ExecutionStatus::Unknown,
            };
            let evidence = format!(
                "multicli-terminal-v1: {} ({}){}",
                event.state.as_str(),
                event
                    .exit
                    .as_ref()
                    .map(|exit| format!(
                        "exit={:?} success={}",
                        exit.code, exit.success
                    ))
                    .unwrap_or_else(|| "exit=unknown".into()),
                event
                    .reason
                    .as_ref()
                    .map(|reason| format!("; {reason}"))
                    .unwrap_or_default()
            );
            if let Err(error) = service.finish_run(
                &event.run_id,
                status,
                Some(&now_iso()),
                Some(&evidence),
            ) {
                // A background thread must never panic: the in-memory state is
                // still correct and the next list/read reports it.
                eprintln!(
                    "[multicli] failed to persist terminal run {}: {}",
                    event.run_id, error
                );
            }
        });

        let config = SessionConfig {
            terminal_id: terminal_id.clone(),
            run_id: run_id.clone(),
            session_id: session_id.clone(),
            agent_id: target.agent_id.clone(),
            installation_id: target.installation_id.clone(),
            title: title.clone(),
            product: target.product.clone(),
            plan: plan.clone(),
            cwd: cwd.clone(),
            args: spec.args.clone(),
            env_names: spec.env.iter().map(|item| item.name.clone()).collect(),
            env: env_pairs(&spec.env),
            limits,
            log,
            cols: spec.cols,
            rows: spec.rows,
            started_at: started_at.clone(),
            launch_paths: Some((self.launch_paths.clone(), terminal_id.clone())),
            notify: Arc::clone(&self.notifier),
            finish,
        };

        let session = match TerminalSession::spawn(config) {
            Ok(session) => session,
            Err(failure) => {
                let _ = cleanup_trampoline(&self.launch_paths, &terminal_id);
                self.fail_run(&run_id, &failure.error);
                return Err(failure.error);
            }
        };

        if let Err(error) = self.link_owned_process(&run_id, &session, &target, spec) {
            // The process exists but the durable link does not: end the owned
            // tree instead of leaving a live process with no recorded owner.
            session.stop(TerminalStopMode::Force, Duration::from_millis(500));
            session.release();
            self.fail_run(&run_id, &error);
            return Err(error);
        }

        let view = session.view();
        self.sessions
            .lock()
            .insert(terminal_id.clone(), Arc::clone(&session));
        Ok(view)
    }

    fn begin_run(
        &self,
        run_id: &str,
        session_id: &str,
        terminal_id: &str,
        target: &PinnedTarget,
        title: &str,
        started_at: &str,
        cwd: &str,
    ) -> Result<(), AppError> {
        self.service.ensure_session(&Session {
            id: session_id.to_string(),
            agent_id: target.agent_id.clone(),
            host_id: self.host_id.clone(),
            source_namespace: format!("{}/terminal/{}", target.agent_id, self.host_id),
            native_id: Some(terminal_id.to_string()),
            origin: SessionOrigin::Managed,
            title: title.to_string(),
        })?;
        // Managed runs have a known cwd. Persist its canonical workspace link
        // so Agent and Workspace navigation address the same session.
        let workspace = self.service.ensure_workspace_at_path(&self.host_id, cwd)?;
        self.service.attach_workspace(&SessionWorkspace {
            session_id: session_id.to_string(),
            workspace_id: workspace.id,
            role: SessionWorkspaceRole::Primary,
            evidence: "multicli-terminal-v1: explicit managed working directory".into(),
        })?;
        self.service.start_run(&Run {
            id: run_id.to_string(),
            session_id: session_id.to_string(),
            installation_id: target.installation_id.clone(),
            transport: RunTransport::Pty,
            ownership: RunOwnership::Managed,
            started_at: Some(started_at.to_string()),
            ended_at: None,
            execution_status: ExecutionStatus::Starting,
            // No structured protocol: without evidence the interaction state
            // stays unknown instead of guessing idle.
            interaction_status: InteractionStatus::Unknown,
            evidence_level: EvidenceLevel::Observed,
            evidence: Some(format!(
                "multicli-terminal-v1: starting; {}",
                chain_note(target.chain)
            )),
        })?;
        Ok(())
    }

    fn fail_run(&self, run_id: &str, error: &AppError) {
        if let Err(failure) = self.service.finish_run(
            run_id,
            ExecutionStatus::Failed,
            Some(&now_iso()),
            Some(&format!(
                "multicli-terminal-v1: failed before running ({}): {}",
                error.code.as_str(),
                error.message
            )),
        ) {
            eprintln!(
                "[multicli] failed to mark terminal run {run_id} as failed: {failure}"
            );
        }
    }

    fn link_owned_process(
        &self,
        run_id: &str,
        session: &Arc<TerminalSession>,
        target: &PinnedTarget,
        spec: &TerminalOpenSpec,
    ) -> Result<(), AppError> {
        let view = session.view();
        let Some(identity) = view.identity.clone() else {
            return Err(AppError::new(
                ErrorCode::Protocol,
                "the terminal has no process identity to link",
            )
            .with_details(serde_json::json!({ "stage": "linkOwnedProcess" })));
        };
        self.service.record_process(&ProcessObservation {
            identity: identity.clone(),
            parent_identity: None,
            executable: Some(host_path(&self.host_id, &session.plan.program)),
            // Never store the user's argv: it can carry prompts or secrets.
            redacted_command: Some(format!(
                "{} [+{} args]",
                target.label,
                spec.args.len()
            )),
            observed_at: now_iso(),
        })?;
        self.service.link_process(&RunProcessLink {
            run_id: run_id.to_string(),
            process_identity: identity,
            link_kind: LinkKind::Owned,
            evidence: "multicli-terminal-v1: the PTY child was created by this application".into(),
        })?;
        // A target can exit while this function is still running (a shim that
        // prints usage and quits). The reader thread already wrote the real end
        // state in that case, so `running` must not overwrite it.
        if session.state().is_terminal() {
            return Ok(());
        }
        self.service.finish_run(
            run_id,
            ExecutionStatus::Running,
            None,
            Some(&format!(
                "multicli-terminal-v1: running; {}",
                chain_note(target.chain)
            )),
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Operations on an existing terminal
    // -----------------------------------------------------------------------

    fn session(&self, terminal_id: &str) -> Result<Arc<TerminalSession>, AppError> {
        self.sessions
            .lock()
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| {
                AppError::not_found("terminal", terminal_id).with_details(serde_json::json!({
                    "stage": "lookup",
                }))
            })
    }

    pub fn read(&self, request: &TerminalReadRequest) -> Result<TerminalReadView, AppError> {
        request.validate()?;
        self.session(&request.terminal_id)?
            .read(request.cursor, request.limit())
    }

    pub fn write(&self, request: &TerminalWriteRequest) -> Result<TerminalWriteView, AppError> {
        request.validate()?;
        self.session(&request.terminal_id)?.write(&request.data)
    }

    pub fn resize(&self, request: &TerminalResizeRequest) -> Result<TerminalResizeView, AppError> {
        request.validate()?;
        let session = self.session(&request.terminal_id)?;
        session.resize(request.cols, request.rows)?;
        Ok(TerminalResizeView {
            terminal_id: session.terminal_id.clone(),
            cols: request.cols,
            rows: request.rows,
        })
    }

    pub fn stop(&self, request: &TerminalStopRequest) -> Result<TerminalStopView, AppError> {
        request.validate()?;
        let session = self.session(&request.terminal_id)?;
        let report = session.stop(request.mode, request.grace());
        Ok(TerminalStopView {
            terminal: session.view(),
            stop: report,
        })
    }

    pub fn close(&self, request: &TerminalCloseRequest) -> Result<TerminalCloseView, AppError> {
        request.validate()?;
        let removed = self.sessions.lock().remove(&request.terminal_id);
        let Some(session) = removed else {
            return Ok(TerminalCloseView {
                terminal_id: request.terminal_id.clone(),
                removed: false,
                stop: TerminalStopReport {
                    requested: request.mode(),
                    interrupt_sent: false,
                    escalated: false,
                    waited_ms: 0,
                    already_terminal: true,
                    refused_reason: None,
                },
                note: "该终端已不在本应用的管理表中；没有停止任何进程。".into(),
            });
        };
        let report = session.stop(request.mode(), request.grace());
        session.release();
        Ok(TerminalCloseView {
            terminal_id: request.terminal_id.clone(),
            removed: true,
            stop: report,
            note: "只结束了本终端自己的进程树；其它终端与外部同款 CLI 不受影响。".into(),
        })
    }

    pub fn list(&self) -> TerminalListView {
        let sessions: Vec<Arc<TerminalSession>> = {
            let map = self.sessions.lock();
            map.values().cloned().collect()
        };
        let mut items: Vec<TerminalView> = sessions.iter().map(|session| session.view()).collect();
        items.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.terminal_id.cmp(&right.terminal_id))
        });
        let active_count = items
            .iter()
            .filter(|item| !item.state.is_terminal())
            .count() as u32;
        let mut note = format!("{STREAM_NOTE} {OWNERSHIP_NOTE}");
        if let Some(extra) = self.settings_note.lock().as_ref() {
            note.push(' ');
            note.push_str(extra);
        }
        TerminalListView {
            schema_version: TERMINAL_SCHEMA_VERSION,
            items,
            limits: self.limits(),
            active_count,
            note,
        }
    }

    pub fn shutdown_all(
        &self,
        request: &TerminalShutdownAllRequest,
    ) -> Result<TerminalShutdownAllView, AppError> {
        let mode = request.mode.unwrap_or(TerminalStopMode::Gentle);
        let grace_ms = request.grace_ms.unwrap_or(2_000);
        if !(100..=60_000).contains(&grace_ms) {
            // A zero grace would make every close fail validation while still
            // reporting success, which reads as "everything stopped".
            return Err(AppError::invalid_argument(
                "graceMs",
                "must be between 100 and 60000",
            ));
        }
        let grace = Duration::from_millis(grace_ms as u64);
        let ids: Vec<String> = {
            let map = self.sessions.lock();
            map.keys().cloned().collect()
        };
        let mut stopped = Vec::new();
        for id in ids {
            let request = TerminalCloseRequest {
                terminal_id: id,
                mode: Some(mode),
                grace_ms: Some(grace.as_millis() as u32),
            };
            match self.close(&request) {
                Ok(view) => stopped.push(view),
                Err(error) => stopped.push(TerminalCloseView {
                    terminal_id: request.terminal_id.clone(),
                    removed: false,
                    stop: TerminalStopReport {
                        requested: mode,
                        interrupt_sent: false,
                        escalated: false,
                        waited_ms: 0,
                        already_terminal: false,
                        refused_reason: Some(error.message),
                    },
                    note: "关闭失败，已保留在管理表中".into(),
                }),
            }
        }
        let remaining = self.sessions.lock().len() as u32;
        Ok(TerminalShutdownAllView {
            stopped,
            remaining,
            note: "只停止本应用创建的终端；外部进程没有被枚举，也没有被按名字结束。".into(),
        })
    }

    pub fn quit_check(&self) -> TerminalQuitCheckView {
        let active = self
            .sessions
            .lock()
            .values()
            .filter(|session| !session.state().is_terminal())
            .count() as u32;
        TerminalQuitCheckView {
            active_count: active,
            needs_confirmation: active > 0,
            note: if active > 0 {
                "仍有本应用创建的终端在运行。退出会先温和停止，超时后才结束自有进程树；外部 CLI 不受影响。"
                    .into()
            } else {
                "没有正在运行的自有终端。".into()
            },
        }
    }

    /// Application exit path: ends every owned tree. Called from the Tauri exit
    /// event, so it must not block on user confirmation and must not wait for
    /// reader threads (the process is about to end).
    pub fn hard_shutdown_all(&self) {
        let sessions: Vec<Arc<TerminalSession>> = {
            let mut map = self.sessions.lock();
            std::mem::take(&mut *map).into_values().collect()
        };
        for session in sessions {
            session.release_without_join();
        }
    }
}

fn env_pairs(env: &[EnvVar]) -> Vec<(String, String)> {
    env.iter()
        .map(|item| (item.name.clone(), item.value.clone()))
        .collect()
}

fn load_limits(root: &Path) -> (TerminalLimits, Option<String>) {
    let mut limits = TerminalLimits::default();
    let path = root.join("terminal-settings.json");
    if !path.exists() {
        return (limits, None);
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            return (
                limits,
                Some(format!(
                    "终端设置文件不可读，已使用默认上限：{error}"
                )),
            )
        }
    };
    match serde_json::from_str::<LimitsFile>(&text) {
        Ok(file) => {
            let update = TerminalLimitsUpdate {
                buffer_bytes: file.buffer_bytes,
                log_bytes: file.log_bytes,
            };
            if update.validate().is_err() {
                return (
                    limits,
                    Some("终端设置文件超出允许范围，已使用默认上限。".into()),
                );
            }
            update.apply(&mut limits);
            (limits, None)
        }
        Err(error) => (
            limits,
            Some(format!("终端设置文件无法解析，已使用默认上限：{error}")),
        ),
    }
}

/// Default log budget used when the settings file has not been written yet.
pub fn default_log_bytes() -> u64 {
    DEFAULT_LOG_BYTES
}

/// Convenience for tests and callers that only have an installation row.
pub fn installation_executable(installation: &AgentInstallation) -> HostPath {
    installation.executable.clone()
}

/// Identity of a session view, for callers that need to assert ownership.
pub fn owned_identity(view: &TerminalView, host_id: &str) -> Option<ProcessIdentity> {
    view.identity
        .as_ref()
        .filter(|identity| identity.host_id == host_id)
        .cloned()
}

/// Kept public for the bootstrap layer: a terminal with a non-terminal state is
/// still readable (its last output remains), but it is not "active".
pub fn is_active(state: TerminalState) -> bool {
    !state.is_terminal()
}
