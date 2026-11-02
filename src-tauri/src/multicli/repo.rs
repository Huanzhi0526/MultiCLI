//! Typed SQL access for the MultiCLI store.
//!
//! All statements are parameterised; list filters use `EXISTS` rather than
//! joins so a session linked to two workspaces still contributes one row to
//! `total` and to the page.

use super::domain::{
    AgentDefinition, AgentInstallation, AgentListItem, Capability, CapabilityEvidenceLevel,
    CapabilityName, CapabilityStatus, EvidenceLevel, ExecutionStatus, Host, HostPath,
    InteractionStatus, LinkKind, Message, ProcessIdentity, ProcessObservation, Run, RunOwnership,
    RunProcessLink, RunTransport, Session, SessionOrigin, SessionRelation, SessionWorkspace,
    SessionWorkspaceRole, Workspace, WorkspaceListItem,
};
use super::error::{AppError, ErrorCode};
use super::store::now_ms;
use rusqlite::{
    params, params_from_iter, types::Value, Connection, OptionalExtension, Row, TransactionBehavior,
};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

fn conversion(error: AppError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn contextual(result: rusqlite::Result<usize>, context: &str) -> Result<usize, AppError> {
    result.map_err(|error| {
        let mut mapped = AppError::from(error);
        if mapped.code == ErrorCode::Conflict {
            mapped.message = format!("{context}: {}", mapped.message);
        }
        mapped
    })
}

fn count_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Hosts
// ---------------------------------------------------------------------------

pub(crate) fn upsert_host(conn: &Connection, host: &Host) -> Result<Host, AppError> {
    conn.execute(
        "INSERT INTO hosts(id, kind, path_style, created_at_ms) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO NOTHING",
        params![
            host.id.as_str(),
            host.kind.as_str(),
            host.path_style.as_str(),
            now_ms()
        ],
    )?;
    let (kind, path_style): (String, String) = conn.query_row(
        "SELECT kind, path_style FROM hosts WHERE id = ?1",
        [host.id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if kind != host.kind.as_str() || path_style != host.path_style.as_str() {
        return Err(AppError::conflict(
            "host",
            "id is already registered with a different kind or path style",
        ));
    }
    Ok(host.clone())
}

// ---------------------------------------------------------------------------
// Agents and installations
// ---------------------------------------------------------------------------

pub(crate) fn upsert_agent(
    conn: &Connection,
    agent: &AgentDefinition,
) -> Result<AgentDefinition, AppError> {
    conn.execute(
        "INSERT INTO agents(id, vendor, display_name, created_at_ms) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET
             vendor = excluded.vendor,
             display_name = excluded.display_name",
        params![
            agent.id.as_str(),
            agent.vendor.as_str(),
            agent.display_name.as_str(),
            now_ms()
        ],
    )?;
    Ok(agent.clone())
}

/// Comparison form of an installation executable. Windows paths are
/// case-insensitive, so the identity is folded while `canonicalPath` itself
/// keeps the case stored on disk.
pub(crate) fn identity_text(value: &str) -> String {
    value
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

/// Derives the schema-v3 executable identity from the frozen installation DTO.
/// A row without a canonical path keeps an explicit unresolved display
/// identity instead of guessing the real executable.
pub(crate) fn executable_identity(installation: &AgentInstallation) -> (String, &'static str) {
    match &installation.executable.canonical_path {
        Some(canonical) => (
            format!("canonical:{}", identity_text(canonical)),
            "canonical",
        ),
        None => (
            format!(
                "unresolved-display:{}",
                identity_text(&installation.executable.display_path)
            ),
            "unresolved-display",
        ),
    }
}

pub(crate) fn upsert_installation(
    conn: &mut Connection,
    installation: &AgentInstallation,
) -> Result<AgentInstallation, AppError> {
    let (identity, identity_kind) = executable_identity(installation);
    upsert_installation_with_identity(conn, installation, &identity, identity_kind)
}

/// Inserts or returns an installation. The complete identity is
/// `host + agent + config profile + executable identity`; a version bump or a
/// re-scan never creates a new row.
pub(crate) fn upsert_installation_with_identity(
    conn: &mut Connection,
    installation: &AgentInstallation,
    identity: &str,
    identity_kind: &str,
) -> Result<AgentInstallation, AppError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(existing) = read_installation(&tx, &installation.id)? {
        let existing_identity = read_installation_identity(&tx, &installation.id)?
            .unwrap_or_else(|| String::from("unresolved-display:"));
        let same_identity = existing.host_id == installation.host_id
            && existing.agent_id == installation.agent_id
            && existing.config_profile_id == installation.config_profile_id
            && existing_identity == identity;
        if !same_identity {
            return Err(AppError::conflict(
                "agent installation",
                "id is already registered to a different host, agent, config profile or executable",
            )
            .with_details(serde_json::json!({
                "installationId": installation.id,
                "existingHostId": existing.host_id,
                "existingAgentId": existing.agent_id,
                "existingConfigProfileId": existing.config_profile_id,
                "requestedHostId": installation.host_id,
                "requestedAgentId": installation.agent_id,
                "requestedConfigProfileId": installation.config_profile_id,
            })));
        }
        tx.commit()?;
        return Ok(existing);
    }
    let by_identity: Option<String> = tx
        .query_row(
            "SELECT id FROM agent_installations
             WHERE host_id = ?1 AND agent_id = ?2 AND config_profile_id = ?3
               AND executable_identity = ?4",
            params![
                installation.host_id.as_str(),
                installation.agent_id.as_str(),
                installation.config_profile_id.as_str(),
                identity,
            ],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing_id) = by_identity {
        return Err(AppError::conflict(
            "agent installation",
            "the complete installation identity is already registered with a different id",
        )
        .with_details(serde_json::json!({
            "requestedId": installation.id,
            "existingId": existing_id,
        })));
    }
    tx.execute(
        "INSERT INTO agent_installations(
             id, host_id, agent_id, executable_display_path, executable_canonical_path,
             executable_identity, executable_identity_kind,
             version, config_profile_id, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            installation.id.as_str(),
            installation.host_id.as_str(),
            installation.agent_id.as_str(),
            installation.executable.display_path.as_str(),
            installation.executable.canonical_path.as_deref(),
            identity,
            identity_kind,
            installation.version.as_deref(),
            installation.config_profile_id.as_str(),
            now_ms(),
        ],
    )?;
    tx.commit()?;
    Ok(installation.clone())
}

pub(crate) fn read_installation_identity(
    conn: &Connection,
    id: &str,
) -> Result<Option<String>, AppError> {
    Ok(conn
        .query_row(
            "SELECT executable_identity FROM agent_installations WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

fn read_installation(conn: &Connection, id: &str) -> Result<Option<AgentInstallation>, AppError> {
    Ok(conn
        .query_row(
            "SELECT id, host_id, agent_id, executable_display_path,
                    executable_canonical_path, version, config_profile_id
             FROM agent_installations WHERE id = ?1",
            [id],
            |row| {
                Ok(AgentInstallation {
                    id: row.get(0)?,
                    host_id: row.get(1)?,
                    agent_id: row.get(2)?,
                    executable: HostPath {
                        host_id: row.get(1)?,
                        display_path: row.get(3)?,
                        canonical_path: row.get::<_, Option<String>>(4)?,
                    },
                    version: row.get::<_, Option<String>>(5)?,
                    config_profile_id: row.get(6)?,
                })
            },
        )
        .optional()?)
}

// ---------------------------------------------------------------------------
// Workspaces
// ---------------------------------------------------------------------------

pub(crate) fn insert_workspace(
    conn: &Connection,
    workspace: &Workspace,
) -> Result<Workspace, AppError> {
    contextual(
        conn.execute(
            "INSERT INTO workspaces(
                 id, host_id, display_path, canonical_path, repo_group_id, worktree_id, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                workspace.id.as_str(),
                workspace.path.host_id.as_str(),
                workspace.path.display_path.as_str(),
                workspace.path.canonical_path.as_deref(),
                workspace.repo_group_id.as_deref(),
                workspace.worktree_id.as_deref(),
                now_ms(),
            ],
        ),
        "workspace already registered for this host and canonical path",
    )?;
    Ok(workspace.clone())
}

fn map_workspace(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    let host_id: String = row.get(1)?;
    Ok(Workspace {
        id: row.get(0)?,
        path: HostPath {
            host_id,
            display_path: row.get(2)?,
            canonical_path: row.get::<_, Option<String>>(3)?,
        },
        repo_group_id: row.get::<_, Option<String>>(4)?,
        worktree_id: row.get::<_, Option<String>>(5)?,
    })
}

pub(crate) fn find_workspace_by_canonical(
    conn: &Connection,
    host_id: &str,
    canonical_path: &str,
) -> Result<Option<Workspace>, AppError> {
    Ok(conn
        .query_row(
            "SELECT id, host_id, display_path, canonical_path, repo_group_id, worktree_id
             FROM workspaces WHERE host_id = ?1 AND canonical_path = ?2",
            params![host_id, canonical_path],
            map_workspace,
        )
        .optional()?)
}

/// Inserts a workspace unless the same `(hostId, canonicalPath)` already
/// exists. The first registration wins; a later alias returns the stored row
/// instead of creating a duplicate.
pub(crate) fn ensure_workspace_by_identity(
    conn: &mut Connection,
    workspace: &Workspace,
) -> Result<Workspace, AppError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored = ensure_workspace_by_identity_in(&tx, workspace)?;
    tx.commit()?;
    Ok(stored)
}

/// Transaction-aware workspace registration used by history import.
pub(crate) fn ensure_workspace_by_identity_in(
    conn: &Connection,
    workspace: &Workspace,
) -> Result<Workspace, AppError> {
    let canonical =
        workspace.path.canonical_path.as_deref().ok_or_else(|| {
            AppError::invalid_argument("canonicalPath", "workspace must be resolved")
        })?;
    if let Some(existing) = find_workspace_by_canonical(conn, &workspace.path.host_id, canonical)? {
        return Ok(existing);
    }
    let inserted = conn.execute(
        "INSERT INTO workspaces(
             id, host_id, display_path, canonical_path, repo_group_id, worktree_id, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            workspace.id.as_str(),
            workspace.path.host_id.as_str(),
            workspace.path.display_path.as_str(),
            canonical,
            workspace.repo_group_id.as_deref(),
            workspace.worktree_id.as_deref(),
            now_ms(),
        ],
    );
    if let Err(error) = inserted {
        let mapped = AppError::from(error);
        if mapped.code == ErrorCode::Conflict {
            if let Some(existing) =
                find_workspace_by_canonical(conn, &workspace.path.host_id, canonical)?
            {
                return Ok(existing);
            }
        }
        return Err(mapped);
    }
    Ok(workspace.clone())
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

fn map_session_row(row: &Row<'_>) -> rusqlite::Result<Session> {
    let origin_text: String = row.get(5)?;
    Ok(Session {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        host_id: row.get(2)?,
        source_namespace: row.get(3)?,
        native_id: row.get::<_, Option<String>>(4)?,
        origin: SessionOrigin::parse_db(&origin_text).map_err(conversion)?,
        title: row.get(6)?,
    })
}

fn same_session_identity(left: &Session, right: &Session) -> bool {
    left.agent_id == right.agent_id
        && left.host_id == right.host_id
        && left.source_namespace == right.source_namespace
        && left.native_id == right.native_id
        && left.origin == right.origin
}

pub(crate) fn ensure_session(
    conn: &mut Connection,
    session: &Session,
    created_at_ms: i64,
) -> Result<Session, AppError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored = ensure_session_in(&tx, session, created_at_ms)?;
    tx.commit()?;
    Ok(stored)
}

/// Transaction-aware variant. History import must commit its messages, FTS
/// rows, provenance and file cursor together, so it calls this instead of
/// opening a nested write transaction (which SQLite would reject).
pub(crate) fn ensure_session_in(
    conn: &Connection,
    session: &Session,
    created_at_ms: i64,
) -> Result<Session, AppError> {
    if let Some(existing) = read_session(conn, &session.id)? {
        if !same_session_identity(&existing, session) {
            return Err(AppError::conflict(
                "session",
                "id is already used by a different native identity, origin, agent or host",
            ));
        }
        return Ok(existing);
    }
    if let Some(native_id) = session.native_id.as_deref() {
        if let Some(existing) = read_session_by_native(conn, &session.source_namespace, native_id)? {
            if existing.agent_id != session.agent_id || existing.host_id != session.host_id {
                return Err(AppError::conflict(
                    "session",
                    "native identity is already attributed to a different agent or host",
                ));
            }
            return Ok(existing);
        }
    }
    conn.execute(
        "INSERT INTO sessions(
             id, agent_id, host_id, source_namespace, native_id, origin, title, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            session.id.as_str(),
            session.agent_id.as_str(),
            session.host_id.as_str(),
            session.source_namespace.as_str(),
            session.native_id.as_deref(),
            session.origin.as_str(),
            session.title.as_str(),
            created_at_ms,
        ],
    )?;
    Ok(session.clone())
}

/// Replaces the display title only when the caller has stronger evidence than
/// the stored title (a native title replaces a first-message guess).
pub(crate) fn set_session_title(
    conn: &Connection,
    session_id: &str,
    title: &str,
) -> Result<(), AppError> {
    if title.trim().is_empty() {
        return Ok(());
    }
    conn.execute(
        "UPDATE sessions SET title = ?2 WHERE id = ?1 AND title <> ?2",
        params![session_id, title],
    )?;
    Ok(())
}

fn read_session(conn: &Connection, id: &str) -> Result<Option<Session>, AppError> {
    Ok(conn
        .query_row(
            "SELECT id, agent_id, host_id, source_namespace, native_id, origin, title
             FROM sessions WHERE id = ?1",
            [id],
            map_session_row,
        )
        .optional()?)
}

fn read_session_by_native(
    conn: &Connection,
    source_namespace: &str,
    native_id: &str,
) -> Result<Option<Session>, AppError> {
    Ok(conn
        .query_row(
            "SELECT id, agent_id, host_id, source_namespace, native_id, origin, title
             FROM sessions WHERE source_namespace = ?1 AND native_id = ?2",
            params![source_namespace, native_id],
            map_session_row,
        )
        .optional()?)
}

pub(crate) fn get_session(conn: &Connection, id: &str) -> Result<Option<Session>, AppError> {
    read_session(conn, id)
}

pub(crate) fn attach_workspace(
    conn: &mut Connection,
    link: &SessionWorkspace,
) -> Result<SessionWorkspace, AppError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored = attach_workspace_in(&tx, link)?;
    tx.commit()?;
    Ok(stored)
}

/// Transaction-aware workspace link. Used by history ingest so the session,
/// message, provenance and workspace evidence land in one commit.
pub(crate) fn attach_workspace_in(
    conn: &Connection,
    link: &SessionWorkspace,
) -> Result<SessionWorkspace, AppError> {
    let hosts: Option<(String, String)> = conn
        .query_row(
            "SELECT s.host_id, w.host_id FROM sessions s
             JOIN workspaces w ON w.id = ?2 WHERE s.id = ?1",
            params![link.session_id, link.workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (session_host, workspace_host) = hosts.ok_or_else(|| {
        AppError::invalid_argument("sessionWorkspace", "session or workspace does not exist")
    })?;
    if session_host != workspace_host {
        return Err(AppError::invalid_argument(
            "workspaceId",
            "workspace and session must belong to the same host",
        ));
    }
    contextual(
        conn.execute(
            "INSERT INTO session_workspaces(session_id, workspace_id, role, evidence, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id, workspace_id) DO UPDATE SET
                 role = excluded.role,
                 evidence = excluded.evidence",
            params![
                link.session_id.as_str(),
                link.workspace_id.as_str(),
                link.role.as_str(),
                link.evidence.as_str(),
                now_ms(),
            ],
        ),
        "session already has a different primary workspace",
    )?;
    conn.query_row(
        "SELECT session_id, workspace_id, role, evidence
         FROM session_workspaces WHERE session_id = ?1 AND workspace_id = ?2",
        params![link.session_id.as_str(), link.workspace_id.as_str()],
        map_session_workspace,
    )
    .optional()?
    .ok_or_else(|| {
        AppError::new(
            ErrorCode::Database,
            "session workspace link disappeared after write",
        )
    })
}

fn map_session_workspace(row: &Row<'_>) -> rusqlite::Result<SessionWorkspace> {
    let role: String = row.get(2)?;
    Ok(SessionWorkspace {
        session_id: row.get(0)?,
        workspace_id: row.get(1)?,
        role: SessionWorkspaceRole::parse_db(&role).map_err(conversion)?,
        evidence: row.get(3)?,
    })
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

fn map_run(row: &Row<'_>) -> rusqlite::Result<Run> {
    let transport: String = row.get(3)?;
    let ownership: String = row.get(4)?;
    let execution: String = row.get(7)?;
    let interaction: String = row.get(8)?;
    let evidence_level: String = row.get(9)?;
    Ok(Run {
        id: row.get(0)?,
        session_id: row.get(1)?,
        installation_id: row.get::<_, Option<String>>(2)?,
        transport: RunTransport::parse_db(&transport).map_err(conversion)?,
        ownership: RunOwnership::parse_db(&ownership).map_err(conversion)?,
        started_at: row.get::<_, Option<String>>(5)?,
        ended_at: row.get::<_, Option<String>>(6)?,
        execution_status: ExecutionStatus::parse_db(&execution).map_err(conversion)?,
        interaction_status: InteractionStatus::parse_db(&interaction).map_err(conversion)?,
        evidence_level: EvidenceLevel::parse_db(&evidence_level).map_err(conversion)?,
        evidence: row.get::<_, Option<String>>(10)?,
    })
}

pub(crate) fn insert_run(conn: &mut Connection, run: &Run) -> Result<Run, AppError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let session_owner: Option<(String, String)> = tx
        .query_row(
            "SELECT agent_id, host_id FROM sessions WHERE id = ?1",
            [run.session_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (session_agent, session_host) = session_owner.ok_or_else(|| {
        AppError::invalid_argument("sessionId", "session does not exist").with_details(
            serde_json::json!({
                "field": "sessionId",
                "reason": "session does not exist",
                "sessionId": run.session_id,
            }),
        )
    })?;
    if let Some(installation_id) = run.installation_id.as_deref() {
        let installation_owner: Option<(String, String)> = tx
            .query_row(
                "SELECT agent_id, host_id FROM agent_installations WHERE id = ?1",
                [installation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (installation_agent, installation_host) = installation_owner.ok_or_else(|| {
            AppError::invalid_argument("installationId", "installation does not exist")
                .with_details(serde_json::json!({
                    "field": "installationId",
                    "reason": "installation does not exist",
                    "installationId": installation_id,
                    "sessionId": run.session_id,
                }))
        })?;
        if installation_agent != session_agent || installation_host != session_host {
            return Err(AppError::invalid_argument(
                "installationId",
                "installation belongs to a different agent or host than the session",
            )
            .with_details(serde_json::json!({
                "field": "installationId",
                "reason": "installation belongs to a different agent or host than the session",
                "installationId": installation_id,
                "installationAgentId": installation_agent,
                "installationHostId": installation_host,
                "sessionAgentId": session_agent,
                "sessionHostId": session_host,
            })));
        }
    }
    if let Some(stored) = read_run(&tx, &run.id)? {
        if stored.session_id != run.session_id
            || stored.transport != run.transport
            || stored.ownership != run.ownership
            || stored.installation_id != run.installation_id
        {
            return Err(AppError::conflict(
                "run",
                "id is already used by a different session, transport, ownership or installation",
            ));
        }
        tx.commit()?;
        return Ok(stored);
    }
    tx.execute(
        "INSERT INTO runs(
             id, session_id, installation_id, transport, ownership, started_at, ended_at,
             execution_status, interaction_status, evidence_level, evidence, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            run.id.as_str(),
            run.session_id.as_str(),
            run.installation_id.as_deref(),
            run.transport.as_str(),
            run.ownership.as_str(),
            run.started_at.as_deref(),
            run.ended_at.as_deref(),
            run.execution_status.as_str(),
            run.interaction_status.as_str(),
            run.evidence_level.as_str(),
            run.evidence.as_deref(),
            now_ms(),
        ],
    )?;
    tx.commit()?;
    Ok(run.clone())
}

fn read_run(conn: &Connection, id: &str) -> Result<Option<Run>, AppError> {
    Ok(conn
        .query_row(
            "SELECT id, session_id, installation_id, transport, ownership, started_at, ended_at,
                    execution_status, interaction_status, evidence_level, evidence
             FROM runs WHERE id = ?1",
            [id],
            map_run,
        )
        .optional()?)
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

fn map_process(row: &Row<'_>) -> rusqlite::Result<ProcessObservation> {
    let host_id: String = row.get(0)?;
    let pid: i64 = row.get(1)?;
    let parent_host: Option<String> = row.get(6)?;
    let parent_pid: Option<i64> = row.get(7)?;
    let parent_created: Option<String> = row.get(8)?;
    let parent_identity = match (parent_host, parent_pid, parent_created) {
        (Some(host_id), Some(pid), Some(creation_time)) => Some(ProcessIdentity {
            host_id,
            pid: u32::try_from(pid).unwrap_or(0),
            creation_time,
        }),
        _ => None,
    };
    let executable_display: Option<String> = row.get(3)?;
    let executable_canonical: Option<String> = row.get(4)?;
    let executable = executable_display.map(|display_path| HostPath {
        host_id: host_id.clone(),
        display_path,
        canonical_path: executable_canonical,
    });
    Ok(ProcessObservation {
        identity: ProcessIdentity {
            host_id,
            pid: u32::try_from(pid).unwrap_or(0),
            creation_time: row.get(2)?,
        },
        parent_identity,
        executable,
        redacted_command: row.get::<_, Option<String>>(5)?,
        observed_at: row.get(9)?,
    })
}

pub(crate) fn observe_process(
    conn: &Connection,
    observation: &ProcessObservation,
) -> Result<ProcessObservation, AppError> {
    let identity = &observation.identity;
    let parent = observation.parent_identity.as_ref();
    let executable = observation.executable.as_ref();
    conn.execute(
        "INSERT INTO processes(
             host_id, pid, creation_time, executable_display_path, executable_canonical_path,
             redacted_command, parent_host_id, parent_pid, parent_creation_time,
             observed_at, first_seen_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(host_id, pid, creation_time) DO NOTHING",
        params![
            identity.host_id.as_str(),
            identity.pid as i64,
            identity.creation_time.as_str(),
            executable.map(|path| path.display_path.as_str()),
            executable.and_then(|path| path.canonical_path.as_deref()),
            observation.redacted_command.as_deref(),
            parent.map(|p| p.host_id.as_str()),
            parent.map(|p| p.pid as i64),
            parent.map(|p| p.creation_time.as_str()),
            observation.observed_at.as_str(),
            now_ms(),
        ],
    )?;
    let stored = read_process(conn, identity)?.ok_or_else(|| {
        AppError::new(
            ErrorCode::Database,
            "process observation disappeared after insert",
        )
    })?;
    Ok(stored)
}

pub(crate) fn read_process(
    conn: &Connection,
    identity: &ProcessIdentity,
) -> Result<Option<ProcessObservation>, AppError> {
    Ok(conn
        .query_row(
            "SELECT host_id, pid, creation_time, executable_display_path,
                    executable_canonical_path, redacted_command, parent_host_id,
                    parent_pid, parent_creation_time, observed_at
             FROM processes WHERE host_id = ?1 AND pid = ?2 AND creation_time = ?3",
            params![
                identity.host_id.as_str(),
                identity.pid as i64,
                identity.creation_time.as_str()
            ],
            map_process,
        )
        .optional()?)
}

/// Persisted discovery row. Separate from `processes` because a discovered
/// instance also carries recognition, freshness and association evidence, and
/// because an `unknown` row may exist without a verified observation.
#[derive(Debug, Clone)]
pub(crate) struct DiscoveryProcessRow {
    pub host_id: String,
    pub pid: u32,
    pub creation_time: String,
    pub agent_id: Option<String>,
    pub installation_id: Option<String>,
    pub product: Option<String>,
    pub entry_kind: Option<String>,
    pub recognition: Option<String>,
    pub evidence_json: String,
    pub state: String,
    pub freshness: String,
    pub state_reason: Option<String>,
    pub executable_display_path: Option<String>,
    pub executable_canonical_path: Option<String>,
    pub redacted_command: Option<String>,
    pub parent: Option<ProcessIdentity>,
    pub parent_note: Option<String>,
    pub source_program: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_evidence: Option<String>,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub last_seen_generation: i64,
}

pub(crate) fn upsert_discovery_process(
    conn: &Connection,
    row: &DiscoveryProcessRow,
) -> Result<(), AppError> {
    let parent = row.parent.as_ref();
    conn.execute(
        "INSERT INTO discovery_processes(
             host_id, pid, creation_time, agent_id, installation_id, product, entry_kind,
             recognition, evidence_json, state, freshness, state_reason,
             executable_display_path, executable_canonical_path, redacted_command,
             parent_host_id, parent_pid, parent_creation_time, parent_note, source_program,
             workspace_id, workspace_evidence, first_seen_at, last_seen_at,
             last_seen_generation, updated_at_ms
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
             ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
         )
         ON CONFLICT(host_id, pid, creation_time) DO UPDATE SET
             agent_id = excluded.agent_id,
             installation_id = excluded.installation_id,
             product = excluded.product,
             entry_kind = excluded.entry_kind,
             recognition = excluded.recognition,
             evidence_json = excluded.evidence_json,
             state = excluded.state,
             freshness = excluded.freshness,
             state_reason = excluded.state_reason,
             executable_display_path = excluded.executable_display_path,
             executable_canonical_path = excluded.executable_canonical_path,
             redacted_command = excluded.redacted_command,
             parent_host_id = excluded.parent_host_id,
             parent_pid = excluded.parent_pid,
             parent_creation_time = excluded.parent_creation_time,
             parent_note = excluded.parent_note,
             source_program = excluded.source_program,
             workspace_id = excluded.workspace_id,
             workspace_evidence = excluded.workspace_evidence,
             last_seen_at = excluded.last_seen_at,
             last_seen_generation = excluded.last_seen_generation,
             updated_at_ms = excluded.updated_at_ms",
        params![
            row.host_id.as_str(),
            row.pid as i64,
            row.creation_time.as_str(),
            row.agent_id.as_deref(),
            row.installation_id.as_deref(),
            row.product.as_deref(),
            row.entry_kind.as_deref(),
            row.recognition.as_deref(),
            row.evidence_json.as_str(),
            row.state.as_str(),
            row.freshness.as_str(),
            row.state_reason.as_deref(),
            row.executable_display_path.as_deref(),
            row.executable_canonical_path.as_deref(),
            row.redacted_command.as_deref(),
            parent.map(|identity| identity.host_id.as_str()),
            parent.map(|identity| identity.pid as i64),
            parent.map(|identity| identity.creation_time.as_str()),
            row.parent_note.as_deref(),
            row.source_program.as_deref(),
            row.workspace_id.as_deref(),
            row.workspace_evidence.as_deref(),
            row.first_seen_at.as_str(),
            row.last_seen_at.as_str(),
            row.last_seen_generation,
            now_ms(),
        ],
    )?;
    Ok(())
}

pub(crate) fn link_run_process(
    conn: &mut Connection,
    link: &RunProcessLink,
) -> Result<RunProcessLink, AppError> {
    let identity = &link.process_identity;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let owner: Option<(String, String)> = tx
        .query_row(
            "SELECT r.ownership, s.host_id FROM runs r
             JOIN sessions s ON s.id = r.session_id WHERE r.id = ?1",
            [link.run_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (ownership, session_host) = owner.ok_or_else(|| {
        AppError::invalid_argument("runId", "run does not exist").with_details(serde_json::json!({
            "field": "runId",
            "reason": "run does not exist",
            "runId": link.run_id,
        }))
    })?;
    let ownership = RunOwnership::parse_db(&ownership)?;
    if ownership == RunOwnership::ExternalObserved && link.link_kind == LinkKind::Owned {
        return Err(AppError::new(
            ErrorCode::AccessDenied,
            "external-observed runs cannot own a process",
        )
        .with_details(serde_json::json!({
            "runId": link.run_id,
            "ownership": ownership.as_str(),
            "requestedLinkKind": link.link_kind.as_str(),
        })));
    }
    if identity.host_id != session_host {
        return Err(AppError::invalid_argument(
            "processIdentity.hostId",
            "process and run session must belong to the same host",
        ));
    }
    let process_exists: i64 = tx.query_row(
        "SELECT COUNT(*) FROM processes WHERE host_id = ?1 AND pid = ?2 AND creation_time = ?3",
        params![
            identity.host_id.as_str(),
            identity.pid as i64,
            identity.creation_time.as_str()
        ],
        |row| row.get(0),
    )?;
    if process_exists == 0 {
        return Err(AppError::invalid_argument(
            "processIdentity",
            "process observation does not exist",
        )
        .with_details(serde_json::json!({
            "field": "processIdentity",
            "reason": "process observation does not exist",
            "hostId": identity.host_id,
            "pid": identity.pid,
            "creationTime": identity.creation_time,
        })));
    }
    tx.execute(
        "INSERT INTO run_process_links(
             run_id, host_id, pid, creation_time, link_kind, evidence, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(run_id, host_id, pid, creation_time) DO NOTHING",
        params![
            link.run_id.as_str(),
            identity.host_id.as_str(),
            identity.pid as i64,
            identity.creation_time.as_str(),
            link.link_kind.as_str(),
            link.evidence.as_str(),
            now_ms(),
        ],
    )?;
    let stored = tx
        .query_row(
            "SELECT run_id, host_id, pid, creation_time, link_kind, evidence
             FROM run_process_links
             WHERE run_id = ?1 AND host_id = ?2 AND pid = ?3 AND creation_time = ?4",
            params![
                link.run_id.as_str(),
                identity.host_id.as_str(),
                identity.pid as i64,
                identity.creation_time.as_str()
            ],
            |row| {
                let kind: String = row.get(4)?;
                Ok(RunProcessLink {
                    run_id: row.get(0)?,
                    process_identity: ProcessIdentity {
                        host_id: row.get(1)?,
                        pid: u32::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                        creation_time: row.get(3)?,
                    },
                    link_kind: LinkKind::parse_db(&kind).map_err(conversion)?,
                    evidence: row.get(5)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::Database,
                "run process link disappeared after write",
            )
        })?;
    tx.commit()?;
    Ok(stored)
}

pub(crate) fn links_for_run(
    conn: &Connection,
    run_id: &str,
) -> Result<Vec<RunProcessLink>, AppError> {
    let mut statement = conn.prepare(
        "SELECT run_id, host_id, pid, creation_time, link_kind, evidence
         FROM run_process_links WHERE run_id = ?1
         ORDER BY creation_time, pid, host_id",
    )?;
    let rows = statement.query_map([run_id], |row| {
        let kind: String = row.get(4)?;
        Ok(RunProcessLink {
            run_id: row.get(0)?,
            process_identity: ProcessIdentity {
                host_id: row.get(1)?,
                pid: u32::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                creation_time: row.get(3)?,
            },
            link_kind: LinkKind::parse_db(&kind).map_err(conversion)?,
            evidence: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------------
// Session relations
// ---------------------------------------------------------------------------

pub(crate) fn insert_relation(
    conn: &Connection,
    relation: &SessionRelation,
) -> Result<(), AppError> {
    conn.execute(
        "INSERT INTO session_relations(
             parent_session_id, child_session_id, kind, confidence, evidence, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(parent_session_id, child_session_id, kind) DO NOTHING",
        params![
            relation.parent_session_id.as_str(),
            relation.child_session_id.as_str(),
            relation.kind.as_str(),
            relation.confidence,
            relation.evidence.as_str(),
            now_ms(),
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

fn map_capability(row: &Row<'_>) -> rusqlite::Result<Capability> {
    let name: String = row.get(2)?;
    let status: String = row.get(3)?;
    let evidence: String = row.get(5)?;
    Ok(Capability {
        agent_id: row.get(0)?,
        installation_id: row.get::<_, Option<String>>(1)?,
        name: CapabilityName::parse_db(&name).map_err(conversion)?,
        status: CapabilityStatus::parse_db(&status).map_err(conversion)?,
        version_range: row.get::<_, Option<String>>(4)?,
        evidence_level: CapabilityEvidenceLevel::parse_db(&evidence).map_err(conversion)?,
        source: row.get(6)?,
        verified_at: row.get::<_, Option<String>>(7)?,
        reason: row.get::<_, Option<String>>(8)?,
    })
}

pub(crate) fn upsert_capability(
    conn: &mut Connection,
    capability: &Capability,
) -> Result<Capability, AppError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(installation_id) = capability.installation_id.as_deref() {
        let owner: Option<String> = tx
            .query_row(
                "SELECT agent_id FROM agent_installations WHERE id = ?1",
                [installation_id],
                |row| row.get(0),
            )
            .optional()?;
        let owner = owner.ok_or_else(|| {
            AppError::invalid_argument("installationId", "installation does not exist")
                .with_details(serde_json::json!({
                    "field": "installationId",
                    "reason": "installation does not exist",
                    "installationId": installation_id,
                    "agentId": capability.agent_id,
                }))
        })?;
        if owner != capability.agent_id {
            return Err(AppError::invalid_argument(
                "installationId",
                "installation belongs to a different agent than the capability",
            )
            .with_details(serde_json::json!({
                "field": "installationId",
                "reason": "installation belongs to a different agent than the capability",
                "installationId": installation_id,
                "installationAgentId": owner,
                "capabilityAgentId": capability.agent_id,
            })));
        }
    }
    let existing: Option<String> = tx
        .query_row(
            "SELECT agent_id FROM capabilities
             WHERE agent_id = ?1 AND name = ?2
               AND COALESCE(installation_id, '') = COALESCE(?3, '')",
            params![
                capability.agent_id.as_str(),
                capability.name.as_str(),
                capability.installation_id.as_deref()
            ],
            |row| row.get(0),
        )
        .optional()?;
    if existing.is_some() {
        tx.execute(
            "UPDATE capabilities
             SET status = ?1, version_range = ?2, evidence_level = ?3, source = ?4,
                 verified_at = ?5, reason = ?6, updated_at_ms = ?7
             WHERE agent_id = ?8 AND name = ?9
               AND COALESCE(installation_id, '') = COALESCE(?10, '')",
            params![
                capability.status.as_str(),
                capability.version_range.as_deref(),
                capability.evidence_level.as_str(),
                capability.source.as_str(),
                capability.verified_at.as_deref(),
                capability.reason.as_deref(),
                now_ms(),
                capability.agent_id.as_str(),
                capability.name.as_str(),
                capability.installation_id.as_deref(),
            ],
        )?;
    } else {
        tx.execute(
            "INSERT INTO capabilities(
                 agent_id, installation_id, name, status, version_range, evidence_level,
                 source, verified_at, reason, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                capability.agent_id.as_str(),
                capability.installation_id.as_deref(),
                capability.name.as_str(),
                capability.status.as_str(),
                capability.version_range.as_deref(),
                capability.evidence_level.as_str(),
                capability.source.as_str(),
                capability.verified_at.as_deref(),
                capability.reason.as_deref(),
                now_ms(),
            ],
        )?;
    }
    tx.commit()?;
    read_capability(conn, capability)?
        .ok_or_else(|| AppError::new(ErrorCode::Database, "capability row could not be read back"))
}

fn read_capability(
    conn: &Connection,
    capability: &Capability,
) -> Result<Option<Capability>, AppError> {
    Ok(conn
        .query_row(
            "SELECT agent_id, installation_id, name, status, version_range, evidence_level,
                    source, verified_at, reason
             FROM capabilities
             WHERE agent_id = ?1 AND name = ?2
               AND COALESCE(installation_id, '') = COALESCE(?3, '')",
            params![
                capability.agent_id.as_str(),
                capability.name.as_str(),
                capability.installation_id.as_deref()
            ],
            map_capability,
        )
        .optional()?)
}

pub(crate) fn list_capabilities(
    conn: &Connection,
    agent_id: &str,
) -> Result<Vec<Capability>, AppError> {
    let mut statement = conn.prepare(
        "SELECT agent_id, installation_id, name, status, version_range, evidence_level,
                source, verified_at, reason
         FROM capabilities WHERE agent_id = ?1
         ORDER BY name, COALESCE(installation_id, '')",
    )?;
    let rows = statement.query_map([agent_id], map_capability)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Stable, paged capability list. The page identity is
/// `(name, COALESCE(installationId, ''))`, which matches the table's unique
/// scope and is stable across restarts.
pub(crate) fn list_capabilities_page(
    conn: &Connection,
    agent_id: &str,
    cursor: Option<(String, String)>,
    fetch_limit: usize,
) -> Result<(Vec<Capability>, u64), AppError> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM capabilities WHERE agent_id = ?1",
        [agent_id],
        |row| row.get(0),
    )?;
    let mut sql = String::from(
        "SELECT agent_id, installation_id, name, status, version_range, evidence_level,
                source, verified_at, reason
         FROM capabilities WHERE agent_id = ?1",
    );
    let mut values = vec![Value::Text(agent_id.to_string())];
    if let Some((name, scope)) = cursor {
        sql.push_str(" AND (name > ? OR (name = ? AND COALESCE(installation_id, '') > ?))");
        values.push(Value::Text(name.clone()));
        values.push(Value::Text(name));
        values.push(Value::Text(scope));
    }
    sql.push_str(" ORDER BY name, COALESCE(installation_id, '') LIMIT ?");
    values.push(Value::Integer(fetch_limit as i64));
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), map_capability)?;
    let rows = rows.collect::<Result<Vec<_>, _>>()?;
    Ok((rows, count_u64(total)))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StoreCounts {
    pub(crate) hosts: u64,
    pub(crate) agents: u64,
    pub(crate) installations: u64,
    pub(crate) workspaces: u64,
    pub(crate) sessions: u64,
    pub(crate) runs: u64,
    pub(crate) messages: u64,
    pub(crate) capabilities: u64,
    pub(crate) processes: u64,
    pub(crate) run_process_links: u64,
}

pub(crate) fn store_counts(conn: &Connection) -> Result<StoreCounts, AppError> {
    fn count(conn: &Connection, table: &str) -> Result<u64, AppError> {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let value: i64 = conn.query_row(&sql, [], |row| row.get(0))?;
        Ok(count_u64(value))
    }
    Ok(StoreCounts {
        hosts: count(conn, "hosts")?,
        agents: count(conn, "agents")?,
        installations: count(conn, "agent_installations")?,
        workspaces: count(conn, "workspaces")?,
        sessions: count(conn, "sessions")?,
        runs: count(conn, "runs")?,
        messages: count(conn, "messages")?,
        capabilities: count(conn, "capabilities")?,
        processes: count(conn, "processes")?,
        run_process_links: count(conn, "run_process_links")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilityBreakdownRow {
    pub(crate) agent_id: String,
    pub(crate) status: CapabilityStatus,
    pub(crate) evidence_level: CapabilityEvidenceLevel,
    pub(crate) count: u64,
}

pub(crate) fn capability_breakdown(
    conn: &Connection,
) -> Result<Vec<CapabilityBreakdownRow>, AppError> {
    let mut statement = conn.prepare(
        "SELECT agent_id, status, evidence_level, COUNT(*)
         FROM capabilities
         GROUP BY agent_id, status, evidence_level
         ORDER BY agent_id, status, evidence_level",
    )?;
    let rows = statement.query_map([], |row| {
        let status: String = row.get(1)?;
        let evidence: String = row.get(2)?;
        Ok(CapabilityBreakdownRow {
            agent_id: row.get(0)?,
            status: CapabilityStatus::parse_db(&status).map_err(conversion)?,
            evidence_level: CapabilityEvidenceLevel::parse_db(&evidence).map_err(conversion)?,
            count: count_u64(row.get(3)?),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

fn map_message(row: &Row<'_>) -> rusqlite::Result<Message> {
    let seq: i64 = row.get(5)?;
    let content_json: String = row.get(7)?;
    let content: JsonValue = serde_json::from_str(&content_json).map_err(|error| {
        conversion(AppError::new(
            ErrorCode::Database,
            format!("stored message content is not valid JSON: {error}"),
        ))
    })?;
    Ok(Message {
        id: row.get(0)?,
        session_id: row.get(1)?,
        run_id: row.get::<_, Option<String>>(2)?,
        native_event_id: row.get::<_, Option<String>>(3)?,
        source_key: row.get(4)?,
        seq: u64::try_from(seq).unwrap_or(0),
        kind: row.get(6)?,
        content,
        source_ref: row.get::<_, Option<String>>(8)?,
        observed_at: row.get(9)?,
    })
}

pub(crate) fn insert_message(
    conn: &mut Connection,
    message: &Message,
) -> Result<Message, AppError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored = insert_message_in(&tx, message)?;
    tx.commit()?;
    Ok(stored)
}

/// Transaction-aware message insert. `inserted` is false when the deterministic
/// id (or the `(session, sourceKey)` uniqueness) already existed, which is what
/// makes a repeated history import idempotent.
pub(crate) fn insert_message_in(
    conn: &Connection,
    message: &Message,
) -> Result<Message, AppError> {
    Ok(insert_message_with_flag_in(conn, message)?.0)
}

pub(crate) fn insert_message_with_flag_in(
    conn: &Connection,
    message: &Message,
) -> Result<(Message, bool), AppError> {
    let content_json = serde_json::to_string(&message.content)?;
    if let Some(run_id) = message.run_id.as_deref() {
        let run_session: Option<String> = conn
            .query_row(
                "SELECT session_id FROM runs WHERE id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        let run_session = run_session.ok_or_else(|| {
            AppError::invalid_argument("runId", "run does not exist").with_details(
                serde_json::json!({
                    "field": "runId",
                    "reason": "run does not exist",
                    "runId": run_id,
                    "sessionId": message.session_id,
                }),
            )
        })?;
        if run_session != message.session_id {
            return Err(AppError::invalid_argument(
                "runId",
                "run belongs to a different session than the message",
            )
            .with_details(serde_json::json!({
                "field": "runId",
                "reason": "run belongs to a different session than the message",
                "runId": run_id,
                "runSessionId": run_session,
                "messageSessionId": message.session_id,
            })));
        }
    }
    let inserted = conn.execute(
        "INSERT INTO messages(
             id, session_id, run_id, native_event_id, source_key, seq, kind,
             content_json, source_ref, observed_at, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT DO NOTHING",
        params![
            message.id.as_str(),
            message.session_id.as_str(),
            message.run_id.as_deref(),
            message.native_event_id.as_deref(),
            message.source_key.as_str(),
            message.seq as i64,
            message.kind.as_str(),
            content_json,
            message.source_ref.as_deref(),
            message.observed_at.as_str(),
            now_ms(),
        ],
    )?;
    if inserted == 1 {
        return Ok((message.clone(), true));
    }
    let stored = match read_message_by_source_key(conn, &message.session_id, &message.source_key)? {
        Some(stored) => stored,
        None => match message.native_event_id.as_deref() {
            Some(native_id) => read_message_by_native(conn, &message.session_id, native_id)?
                .ok_or_else(|| {
                    AppError::conflict(
                        "message",
                        "source key is already used by a different native event",
                    )
                })?,
            None => {
                return Err(AppError::conflict(
                    "message",
                    "message id is already used by a different source key",
                ))
            }
        },
    };
    Ok((stored, false))
}

fn read_message_by_source_key(
    conn: &Connection,
    session_id: &str,
    source_key: &str,
) -> Result<Option<Message>, AppError> {
    Ok(conn
        .query_row(
            "SELECT id, session_id, run_id, native_event_id, source_key, seq, kind,
                    content_json, source_ref, observed_at
             FROM messages WHERE session_id = ?1 AND source_key = ?2",
            params![session_id, source_key],
            map_message,
        )
        .optional()?)
}

fn read_message_by_native(
    conn: &Connection,
    session_id: &str,
    native_event_id: &str,
) -> Result<Option<Message>, AppError> {
    Ok(conn
        .query_row(
            "SELECT id, session_id, run_id, native_event_id, source_key, seq, kind,
                    content_json, source_ref, observed_at
             FROM messages WHERE session_id = ?1 AND native_event_id = ?2",
            params![session_id, native_event_id],
            map_message,
        )
        .optional()?)
}

// ---------------------------------------------------------------------------
// List queries
// ---------------------------------------------------------------------------

pub(crate) struct SessionFilter<'a> {
    pub(crate) workspace_id: Option<&'a str>,
    pub(crate) agent_id: Option<&'a str>,
    pub(crate) origin: Option<SessionOrigin>,
}

impl SessionFilter<'_> {
    fn clause(&self) -> (String, Vec<Value>) {
        let mut sql = String::from(" WHERE 1 = 1");
        let mut values = Vec::new();
        if let Some(workspace_id) = self.workspace_id {
            sql.push_str(
                " AND EXISTS (
                    SELECT 1 FROM session_workspaces swf
                    WHERE swf.session_id = s.id AND swf.workspace_id = ?
                )",
            );
            values.push(Value::Text(workspace_id.to_string()));
        }
        if let Some(agent_id) = self.agent_id {
            sql.push_str(" AND s.agent_id = ?");
            values.push(Value::Text(agent_id.to_string()));
        }
        if let Some(origin) = self.origin {
            sql.push_str(" AND s.origin = ?");
            values.push(Value::Text(origin.as_str().to_string()));
        }
        (sql, values)
    }
}

pub(crate) struct SessionListRow {
    pub(crate) session: Session,
    pub(crate) created_at_ms: i64,
    pub(crate) message_count: u64,
    pub(crate) run_count: u64,
    pub(crate) primary_workspace_id: Option<String>,
}

pub(crate) fn list_sessions(
    conn: &Connection,
    filter: &SessionFilter<'_>,
    cursor: Option<(i64, String)>,
    fetch_limit: usize,
) -> Result<(Vec<SessionListRow>, u64), AppError> {
    let (clause, filter_values) = filter.clause();
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM sessions s{clause}"),
        params_from_iter(filter_values.iter()),
        |row| row.get(0),
    )?;

    let mut sql = String::from(
        "SELECT s.id, s.agent_id, s.host_id, s.source_namespace, s.native_id, s.origin,
                s.title, s.created_at_ms,
                (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id),
                (SELECT COUNT(*) FROM runs r WHERE r.session_id = s.id),
                (SELECT sw.workspace_id FROM session_workspaces sw
                  WHERE sw.session_id = s.id AND sw.role = 'primary' LIMIT 1)
         FROM sessions s",
    );
    sql.push_str(&clause);
    let mut values = filter_values;
    if let Some((created_at_ms, id)) = cursor {
        sql.push_str(" AND (s.created_at_ms < ? OR (s.created_at_ms = ? AND s.id < ?))");
        values.push(Value::Integer(created_at_ms));
        values.push(Value::Integer(created_at_ms));
        values.push(Value::Text(id));
    }
    sql.push_str(" ORDER BY s.created_at_ms DESC, s.id DESC LIMIT ?");
    values.push(Value::Integer(fetch_limit as i64));

    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), |row| {
        Ok(SessionListRow {
            session: map_session_row(row)?,
            created_at_ms: row.get(7)?,
            message_count: count_u64(row.get(8)?),
            run_count: count_u64(row.get(9)?),
            primary_workspace_id: row.get(10)?,
        })
    })?;
    let rows = rows.collect::<Result<Vec<_>, _>>()?;
    Ok((rows, count_u64(total)))
}

pub(crate) fn workspace_ids_for_sessions(
    conn: &Connection,
    session_ids: &[String],
) -> Result<HashMap<String, Vec<String>>, AppError> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    if session_ids.is_empty() {
        return Ok(map);
    }
    let placeholders = std::iter::repeat("?")
        .take(session_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT session_id, workspace_id FROM session_workspaces
         WHERE session_id IN ({placeholders})
         ORDER BY session_id, workspace_id"
    );
    let values = session_ids
        .iter()
        .map(|id| Value::Text(id.clone()))
        .collect::<Vec<_>>();
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (session_id, workspace_id) = row?;
        map.entry(session_id).or_default().push(workspace_id);
    }
    Ok(map)
}

pub(crate) fn list_workspaces(
    conn: &Connection,
    cursor: Option<(String, String)>,
    fetch_limit: usize,
) -> Result<(Vec<WorkspaceListItem>, u64), AppError> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))?;
    let mut sql = String::from(
        "SELECT w.id, w.host_id, w.display_path, w.canonical_path, w.repo_group_id,
                w.worktree_id,
                (SELECT COUNT(DISTINCT sw.session_id) FROM session_workspaces sw
                  WHERE sw.workspace_id = w.id)
         FROM workspaces w",
    );
    let mut values = Vec::new();
    if let Some((display_path, id)) = cursor {
        sql.push_str(" WHERE (w.display_path > ? OR (w.display_path = ? AND w.id > ?))");
        values.push(Value::Text(display_path.clone()));
        values.push(Value::Text(display_path));
        values.push(Value::Text(id));
    }
    sql.push_str(" ORDER BY w.display_path, w.id LIMIT ?");
    values.push(Value::Integer(fetch_limit as i64));
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), |row| {
        Ok(WorkspaceListItem {
            workspace: map_workspace(row)?,
            session_count: count_u64(row.get(6)?),
        })
    })?;
    let rows = rows.collect::<Result<Vec<_>, _>>()?;
    Ok((rows, count_u64(total)))
}

pub(crate) fn list_agents(
    conn: &Connection,
    cursor: Option<(String, String)>,
    fetch_limit: usize,
) -> Result<(Vec<AgentListItem>, u64), AppError> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM agents", [], |row| row.get(0))?;
    let mut sql = String::from(
        "SELECT a.id, a.vendor, a.display_name,
                (SELECT COUNT(*) FROM agent_installations i WHERE i.agent_id = a.id),
                (SELECT COUNT(*) FROM sessions s WHERE s.agent_id = a.id)
         FROM agents a",
    );
    let mut values = Vec::new();
    if let Some((display_name, id)) = cursor {
        sql.push_str(" WHERE (a.display_name > ? OR (a.display_name = ? AND a.id > ?))");
        values.push(Value::Text(display_name.clone()));
        values.push(Value::Text(display_name));
        values.push(Value::Text(id));
    }
    sql.push_str(" ORDER BY a.display_name, a.id LIMIT ?");
    values.push(Value::Integer(fetch_limit as i64));
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), |row| {
        Ok(AgentListItem {
            agent: AgentDefinition {
                id: row.get(0)?,
                vendor: row.get(1)?,
                display_name: row.get(2)?,
            },
            installation_count: count_u64(row.get(3)?),
            session_count: count_u64(row.get(4)?),
        })
    })?;
    let rows = rows.collect::<Result<Vec<_>, _>>()?;
    Ok((rows, count_u64(total)))
}

pub(crate) fn session_workspaces(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<SessionWorkspace>, AppError> {
    let mut statement = conn.prepare(
        "SELECT session_id, workspace_id, role, evidence
         FROM session_workspaces WHERE session_id = ?1
         ORDER BY CASE role WHEN 'primary' THEN 0 ELSE 1 END, workspace_id",
    )?;
    let rows = statement.query_map([session_id], map_session_workspace)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(crate) fn runs_for_session(conn: &Connection, session_id: &str) -> Result<Vec<Run>, AppError> {
    let mut statement = conn.prepare(
        "SELECT id, session_id, installation_id, transport, ownership, started_at, ended_at,
                execution_status, interaction_status, evidence_level, evidence
         FROM runs WHERE session_id = ?1
         ORDER BY created_at_ms, id",
    )?;
    let rows = statement.query_map([session_id], map_run)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(crate) fn messages_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<Message>, AppError> {
    let mut statement = conn.prepare(
        "SELECT id, session_id, run_id, native_event_id, source_key, seq, kind,
                content_json, source_ref, observed_at
         FROM messages WHERE session_id = ?1
         ORDER BY observed_at, seq, id",
    )?;
    let rows = statement.query_map([session_id], map_message)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------------
// Bounded message reads (S03)
// ---------------------------------------------------------------------------

/// Stable position of a message inside its session timeline. Timestamps are
/// stored as canonical UTC with milliseconds, so a plain string comparison is
/// also a chronological comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessagePosition {
    pub(crate) observed_at: String,
    pub(crate) seq: u64,
    pub(crate) id: String,
}

const MESSAGE_COLUMNS: &str =
    "id, session_id, run_id, native_event_id, source_key, seq, kind, content_json, source_ref, observed_at";

pub(crate) fn message_position(
    conn: &Connection,
    session_id: &str,
    message_id: &str,
) -> Result<Option<(MessagePosition, u64)>, AppError> {
    let row: Option<(String, i64, String)> = conn
        .query_row(
            "SELECT observed_at, seq, id FROM messages WHERE session_id = ?1 AND id = ?2",
            params![session_id, message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((observed_at, seq, id)) = row else {
        return Ok(None);
    };
    let offset: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages
         WHERE session_id = ?1 AND (
             observed_at < ?2
             OR (observed_at = ?2 AND (
                 seq < ?3 OR (seq = ?3 AND id < ?4)
             ))
         )",
        params![session_id, observed_at.as_str(), seq, id.as_str()],
        |row| row.get(0),
    )?;
    Ok(Some((
        MessagePosition {
            observed_at,
            seq: u64::try_from(seq).unwrap_or(0),
            id,
        },
        u64::try_from(offset).unwrap_or(0),
    )))
}

/// Forward page of messages in `(observedAt, seq, id)` order.
pub(crate) fn session_messages_page(
    conn: &Connection,
    session_id: &str,
    cursor: Option<&MessagePosition>,
    offset: Option<u64>,
    fetch_limit: usize,
) -> Result<(Vec<Message>, u64), AppError> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )?;
    let mut sql = format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE session_id = ?1");
    let mut values = vec![Value::Text(session_id.to_string())];
    if let Some(position) = cursor {
        sql.push_str(
            " AND (observed_at > ? OR (observed_at = ? AND (
                 seq > ? OR (seq = ? AND id > ?)
             )))",
        );
        values.push(Value::Text(position.observed_at.clone()));
        values.push(Value::Text(position.observed_at.clone()));
        values.push(Value::Integer(position.seq as i64));
        values.push(Value::Integer(position.seq as i64));
        values.push(Value::Text(position.id.clone()));
    }
    sql.push_str(" ORDER BY observed_at, seq, id LIMIT ?");
    values.push(Value::Integer(fetch_limit as i64));
    if let Some(offset) = offset {
        sql.push_str(" OFFSET ?");
        values.push(Value::Integer(offset.min(i64::MAX as u64) as i64));
    }
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), map_message)?;
    let rows = rows.collect::<Result<Vec<_>, _>>()?;
    Ok((rows, count_u64(total)))
}

pub(crate) fn message_kind_counts(
    conn: &Connection,
    session_id: &str,
    limit: usize,
) -> Result<Vec<(String, u64)>, AppError> {
    let mut statement = conn.prepare(
        "SELECT kind, COUNT(*) FROM messages WHERE session_id = ?1
         GROUP BY kind ORDER BY COUNT(*) DESC, kind LIMIT ?2",
    )?;
    let rows = statement.query_map(params![session_id, limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, count_u64(row.get(1)?)))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(crate) fn message_time_bounds(
    conn: &Connection,
    session_id: &str,
) -> Result<(Option<String>, Option<String>), AppError> {
    Ok(conn.query_row(
        "SELECT MIN(observed_at), MAX(observed_at) FROM messages WHERE session_id = ?1",
        [session_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

pub(crate) fn workspace_by_id(
    conn: &Connection,
    workspace_id: &str,
) -> Result<Option<Workspace>, AppError> {
    Ok(conn
        .query_row(
            "SELECT id, host_id, display_path, canonical_path, repo_group_id, worktree_id
             FROM workspaces WHERE id = ?1",
            [workspace_id],
            map_workspace,
        )
        .optional()?)
}

pub(crate) fn session_workspace_rows(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<(SessionWorkspace, Option<Workspace>)>, AppError> {
    let mut statement = conn.prepare(
        "SELECT sw.session_id, sw.workspace_id, sw.role, sw.evidence,
                w.id, w.host_id, w.display_path, w.canonical_path, w.repo_group_id, w.worktree_id
         FROM session_workspaces sw
         LEFT JOIN workspaces w ON w.id = sw.workspace_id
         WHERE sw.session_id = ?1
         ORDER BY CASE sw.role WHEN 'primary' THEN 0 ELSE 1 END, sw.workspace_id",
    )?;
    let rows = statement.query_map([session_id], |row| {
        let link = map_session_workspace(row)?;
        let workspace = match row.get::<_, Option<String>>(4)? {
            Some(id) => Some(Workspace {
                id,
                path: HostPath {
                    host_id: row.get(5)?,
                    display_path: row.get(6)?,
                    canonical_path: row.get::<_, Option<String>>(7)?,
                },
                repo_group_id: row.get::<_, Option<String>>(8)?,
                worktree_id: row.get::<_, Option<String>>(9)?,
            }),
            None => None,
        };
        Ok((link, workspace))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Which sources and files actually produced this session's messages.
pub(crate) fn history_provenance(
    conn: &Connection,
    session_id: &str,
) -> Result<(Vec<String>, u64, Vec<u32>), AppError> {
    let mut statement = conn.prepare(
        "SELECT o.source_id, COUNT(DISTINCT o.file_identity), MAX(o.parser_version)
         FROM history_message_origins o
         JOIN messages m ON m.id = o.message_id
         WHERE m.session_id = ?1
         GROUP BY o.source_id
         ORDER BY o.source_id",
    )?;
    let rows = statement.query_map([session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            count_u64(row.get::<_, i64>(1)?),
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut sources = Vec::new();
    let mut files = 0_u64;
    let mut versions = Vec::new();
    for row in rows {
        let (source_id, file_count, parser_version) = row?;
        sources.push(source_id);
        files += file_count;
        let version = u32::try_from(parser_version).unwrap_or(0);
        if !versions.contains(&version) {
            versions.push(version);
        }
    }
    versions.sort_unstable();
    Ok((sources, files, versions))
}

/// Runs that can still write this session. A finished run is not a conflict;
/// an active one definitely is.
pub(crate) fn active_runs_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<Run>, AppError> {
    let mut statement = conn.prepare(
        "SELECT id, session_id, installation_id, transport, ownership, started_at, ended_at,
                execution_status, interaction_status, evidence_level, evidence
         FROM runs
         WHERE session_id = ?1 AND execution_status IN ('starting', 'running', 'stopping')
         ORDER BY created_at_ms, id",
    )?;
    let rows = statement.query_map([session_id], map_run)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(crate) fn runs_page_for_session(
    conn: &Connection,
    session_id: &str,
    limit: usize,
) -> Result<(Vec<Run>, u64), AppError> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM runs WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )?;
    let mut statement = conn.prepare(
        "SELECT id, session_id, installation_id, transport, ownership, started_at, ended_at,
                execution_status, interaction_status, evidence_level, evidence
         FROM runs WHERE session_id = ?1
         ORDER BY created_at_ms, id LIMIT ?2",
    )?;
    let rows = statement.query_map(params![session_id, limit as i64], map_run)?;
    Ok((
        rows.collect::<Result<Vec<_>, _>>()?,
        count_u64(total),
    ))
}

/// Installations of one agent on one host, used by resume-target resolution.
/// The caller never guesses a target from an installation that is not stored.
pub(crate) fn installations_for_agent(
    conn: &Connection,
    host_id: &str,
    agent_id: &str,
) -> Result<Vec<AgentInstallation>, AppError> {
    let mut statement = conn.prepare(
        "SELECT id, host_id, agent_id, executable_display_path,
                executable_canonical_path, version, config_profile_id
         FROM agent_installations
         WHERE host_id = ?1 AND agent_id = ?2
         ORDER BY created_at_ms, id",
    )?;
    let rows = statement.query_map(params![host_id, agent_id], |row| {
        Ok(AgentInstallation {
            id: row.get(0)?,
            host_id: row.get(1)?,
            agent_id: row.get(2)?,
            executable: HostPath {
                host_id: row.get(1)?,
                display_path: row.get(3)?,
                canonical_path: row.get::<_, Option<String>>(4)?,
            },
            version: row.get::<_, Option<String>>(5)?,
            config_profile_id: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Live external instances of one product. They are *not* linked to a session
/// (`sessionId` is null in S02), so they can only ever downgrade a resume
/// target to `unknown`, never prove it is free.
pub(crate) fn alive_discovery_processes(
    conn: &Connection,
    agent_id: &str,
) -> Result<Vec<(u32, String, Option<String>)>, AppError> {
    let mut statement = conn.prepare(
        "SELECT pid, creation_time, workspace_id FROM discovery_processes
         WHERE agent_id = ?1 AND state = 'alive' AND freshness = 'fresh'
         ORDER BY last_seen_at DESC, pid",
    )?;
    let rows = statement.query_map([agent_id], |row| {
        Ok((
            u32::try_from(row.get::<_, i64>(0)?).unwrap_or(0),
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
