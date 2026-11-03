//! Resume-target resolution — parse only, never execute.
//!
//! S03 has no execution. This module answers one question with evidence: could
//! this historical session be continued later, and what argues against it right
//! now? It never starts a process, never writes a CLI file, never sends a prompt
//! and never creates a Run.
//!
//! Absence of a matching process is **not** evidence of idleness: S02's process
//! view has no native session id, so a live instance of the same product cannot
//! be proven unrelated either. That case degrades to `unknown` with an explicit
//! reason instead of a false `resolved`.

use super::model::{
    product_for_agent, ResumeConflictView, ResumeTargetView, RESUME_NOTE,
};
use super::repo;
use crate::multicli::repo as domain_repo;
use crate::multicli::MultiCliService;
use crate::multicli::error::AppError;
use serde_json::json;

pub fn resolve_resume_target(
    service: &MultiCliService,
    session_id: &str,
) -> Result<ResumeTargetView, AppError> {
    if session_id.trim().is_empty() {
        return Err(AppError::invalid_argument(
            "sessionId",
            "must not be empty",
        ));
    }
    let conn = service.store().connection().lock();
    let session = domain_repo::get_session(&conn, session_id)?
        .ok_or_else(|| AppError::not_found("session", session_id))?;

    let fact = repo::fact_for_session(&conn, session_id)?;
    let source = match &fact {
        Some((source_id, _, _)) => repo::read_source(&conn, source_id)?,
        None => None,
    };
    let product = product_for_agent(&session.agent_id);
    let workspaces = domain_repo::session_workspace_rows(&conn, session_id)?
        .into_iter()
        .filter_map(|(_, workspace)| workspace.map(|workspace| workspace.path))
        .collect::<Vec<_>>();

    let mut conflicts: Vec<ResumeConflictView> = Vec::new();
    let mut evidence: Vec<String> = Vec::new();
    let mut status = "resolved".to_string();
    let mut reason = "a native id and a product are known".to_string();

    if session.native_id.is_none() {
        status = "unsupported".to_string();
        reason = "the session has no native id, so no product target can be named".to_string();
        conflicts.push(ResumeConflictView {
            kind: "missing-native-id".to_string(),
            detail: "nativeId is null".to_string(),
        });
    }
    if product.is_none() {
        status = "unsupported".to_string();
        reason = "no resume target is known for this agent".to_string();
        evidence.push("agent-not-in-history-products".to_string());
    }

    let active_runs = domain_repo::active_runs_for_session(&conn, session_id)?;
    for run in &active_runs {
        conflicts.push(ResumeConflictView {
            kind: "active-run".to_string(),
            detail: format!(
                "run {} is {} ({} ownership)",
                run.id,
                run.execution_status.as_str(),
                run.ownership.as_str()
            ),
        });
        evidence.push("active-run".to_string());
    }
    if !active_runs.is_empty() {
        status = "blocked".to_string();
        reason = "an active run can still write this session".to_string();
    }

    // Live external instances cannot be tied to this native session; they are
    // reported as a real uncertainty instead of being ignored.
    let primary_workspace = workspaces
        .first()
        .and_then(|path| path.canonical_path.clone());
    let alive = domain_repo::alive_discovery_processes(&conn, &session.agent_id)?;
    let mut related_live = 0_usize;
    for (pid, creation_time, workspace_id) in &alive {
        let same_workspace = match (&primary_workspace, workspace_id) {
            (Some(canonical), Some(workspace_id)) => {
                match domain_repo::workspace_by_id(&conn, workspace_id)? {
                    Some(workspace) => {
                        workspace.path.canonical_path.as_deref() == Some(canonical.as_str())
                    }
                    None => false,
                }
            }
            _ => false,
        };
        if same_workspace {
            related_live += 1;
            evidence.push("live-process-same-workspace".to_string());
            conflicts.push(ResumeConflictView {
                kind: "live-external-process".to_string(),
                detail: format!(
                    "a live {} instance (pid {pid}, creation {creation_time}) is in the session workspace, but has no verified session link",
                    session.agent_id
                ),
            });
        }
    }
    if related_live > 0 && status == "resolved" {
        status = "unknown".to_string();
        reason =
            "a live instance of the same product is in this workspace; its session is not verifiable, so idleness cannot be claimed"
                .to_string();
    } else if !alive.is_empty() && status == "resolved" {
        status = "unknown".to_string();
        reason = "live instances of this product exist; none can be matched to this native session"
            .to_string();
        evidence.push("live-process-unlinked".to_string());
    }

    let installations = match product {
        Some(product) => {
            domain_repo::installations_for_agent(&conn, &session.host_id, product.agent_id)?
        }
        None => Vec::new(),
    };
    let preferred_installation = source
        .as_ref()
        .and_then(|source| source.installation_id.clone())
        .and_then(|id| installations.iter().find(|item| item.id == id).cloned())
        .or_else(|| installations.first().cloned());
    if preferred_installation.is_none() && status == "resolved" {
        status = "unknown".to_string();
        reason = "no confirmed installation is registered for this product".to_string();
        evidence.push("installation-not-confirmed".to_string());
    }

    let config_profile_id = source.as_ref().map(|source| source.config_profile_id.clone());
    let executable = preferred_installation
        .as_ref()
        .map(|installation| installation.executable.clone());

    let execution_available = status == "resolved";
    Ok(ResumeTargetView {
        session_id: session.id.clone(),
        agent_id: session.agent_id.clone(),
        product: product.map(|product| product.product.to_string()),
        native_id: session.native_id.clone(),
        source_namespace: session.source_namespace.clone(),
        config_profile_id,
        status,
        reason,
        installation_id: preferred_installation.as_ref().map(|item| item.id.clone()),
        executable,
        workspaces,
        conflicts,
        evidence,
        execution_available,
        note: RESUME_NOTE.to_string(),
    })
}

/// Kept separate so the diagnostics/UI can state the exact guarantee without
/// reading the implementation.
pub fn resolution_scope() -> serde_json::Value {
    json!({
        "spawnsProcess": false,
        "sendsPrompt": false,
        "writesCliFiles": false,
        "createsRun": false,
        "executionStage": "S05",
    })
}
