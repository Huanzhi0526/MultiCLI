//! Internal read/write service for the S01 navigation data layer.
//!
//! S01-02 will expose these operations through `multicli_*` IPC. This batch
//! deliberately registers no Tauri command and creates no rows on startup.

use super::diagnostics::{
    build_diagnostics, iso_timestamp, write_diagnostics, CapabilityCounts, DiagnosticsCounts,
    DiagnosticsExport,
};
use super::domain::{
    AgentDefinition, AgentInstallation, AgentListItem, Capability, CapabilityQuery, Host, Message,
    Page, PageRequest, ProcessIdentity, ProcessObservation, Run, RunProcessLink, Session,
    SessionDetail, SessionQuery, SessionRelation, SessionSummary, SessionWorkspace, Workspace,
    WorkspaceListItem,
};
use super::error::AppError;
use super::path::{self, ResolvedWorkspacePath};
use super::repo;
use super::store::{now_ms, MigrationReport, MultiCliStore};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
#[cfg(test)]
use parking_lot::MutexGuard;
#[cfg(test)]
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CursorPayload {
    sort: String,
    key: String,
    id: String,
}

fn encode_cursor(sort: &str, key: &str, id: &str) -> String {
    let payload = CursorPayload {
        sort: sort.to_string(),
        key: key.to_string(),
        id: id.to_string(),
    };
    let bytes = serde_json::to_vec(&payload).unwrap_or_default();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_cursor(cursor: &str, expected_sort: &str) -> Result<(String, String), AppError> {
    if cursor.trim().is_empty() {
        return Err(AppError::invalid_argument("cursor", "must not be empty"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| AppError::invalid_argument("cursor", "is not a valid pagination cursor"))?;
    let payload: CursorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::invalid_argument("cursor", "is not a valid pagination cursor"))?;
    if payload.sort != expected_sort {
        return Err(AppError::invalid_argument(
            "cursor",
            format!("cursor belongs to {}, not {expected_sort}", payload.sort),
        ));
    }
    Ok((payload.key, payload.id))
}

fn paginate<T, R>(
    mut rows: Vec<R>,
    total: u64,
    limit: u32,
    sort: &str,
    cursor_of: impl Fn(&R) -> (String, String),
    mut map: impl FnMut(R) -> T,
) -> Page<T> {
    let limit = limit as usize;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if has_more {
        rows.last().map(|row| {
            let (key, id) = cursor_of(row);
            encode_cursor(sort, &key, &id)
        })
    } else {
        None
    };
    Page {
        items: rows.into_iter().map(&mut map).collect(),
        next_cursor,
        total,
    }
}

pub struct MultiCliService {
    store: MultiCliStore,
}

impl MultiCliService {
    /// Opens the isolated `multicli.db` under `data_root`. The caller must pass
    /// the MultiCLI profile root explicitly; this module has no default path
    /// and never reaches for the upstream application database.
    pub fn open(data_root: impl AsRef<Path>) -> Result<Self, AppError> {
        Ok(Self {
            store: MultiCliStore::open(data_root)?,
        })
    }

    pub fn store(&self) -> &MultiCliStore {
        &self.store
    }

    pub fn schema_version(&self) -> i64 {
        self.store.schema_version()
    }

    pub fn database_path(&self) -> &Path {
        self.store.database_path()
    }

    pub fn migration_report(&self) -> &MigrationReport {
        self.store.migration_report()
    }

    pub fn journal_mode(&self) -> Result<String, AppError> {
        self.store.journal_mode()
    }

    pub fn foreign_keys_enabled(&self) -> Result<bool, AppError> {
        self.store.foreign_keys_enabled()
    }

    // -----------------------------------------------------------------------
    // Writes
    // -----------------------------------------------------------------------

    pub fn register_host(&self, host: &Host) -> Result<Host, AppError> {
        host.validate()?;
        let conn = self.store.connection().lock();
        repo::upsert_host(&conn, host)
    }

    pub fn register_agent(&self, agent: &AgentDefinition) -> Result<AgentDefinition, AppError> {
        agent.validate()?;
        let conn = self.store.connection().lock();
        repo::upsert_agent(&conn, agent)
    }

    pub fn register_installation(
        &self,
        installation: &AgentInstallation,
    ) -> Result<AgentInstallation, AppError> {
        installation.validate()?;
        let mut conn = self.store.connection().lock();
        repo::upsert_installation(&mut conn, installation)
    }

    pub fn register_workspace(&self, workspace: &Workspace) -> Result<Workspace, AppError> {
        workspace.validate()?;
        let conn = self.store.connection().lock();
        repo::insert_workspace(&conn, workspace)
    }

    pub fn ensure_session(&self, session: &Session) -> Result<Session, AppError> {
        self.ensure_session_at(session, now_ms())
    }

    pub(crate) fn ensure_session_at(
        &self,
        session: &Session,
        created_at_ms: i64,
    ) -> Result<Session, AppError> {
        session.validate()?;
        let mut conn = self.store.connection().lock();
        repo::ensure_session(&mut conn, session, created_at_ms)
    }

    pub fn attach_workspace(&self, link: &SessionWorkspace) -> Result<SessionWorkspace, AppError> {
        link.validate()?;
        let mut conn = self.store.connection().lock();
        repo::attach_workspace(&mut conn, link)
    }

    pub fn start_run(&self, run: &Run) -> Result<Run, AppError> {
        run.validate()?;
        let mut conn = self.store.connection().lock();
        repo::insert_run(&mut conn, run)
    }

    pub fn record_process(
        &self,
        observation: &ProcessObservation,
    ) -> Result<ProcessObservation, AppError> {
        observation.validate()?;
        let conn = self.store.connection().lock();
        repo::observe_process(&conn, observation)
    }

    pub fn link_process(&self, link: &RunProcessLink) -> Result<RunProcessLink, AppError> {
        link.validate()?;
        let mut conn = self.store.connection().lock();
        repo::link_run_process(&mut conn, link)
    }

    pub fn record_message(&self, message: &Message) -> Result<Message, AppError> {
        message.validate()?;
        let mut conn = self.store.connection().lock();
        repo::insert_message(&mut conn, message)
    }

    pub fn relate_sessions(&self, relation: &SessionRelation) -> Result<(), AppError> {
        relation.validate()?;
        let conn = self.store.connection().lock();
        repo::insert_relation(&conn, relation)
    }

    pub fn declare_capability(&self, capability: &Capability) -> Result<Capability, AppError> {
        capability.validate()?;
        let mut conn = self.store.connection().lock();
        repo::upsert_capability(&mut conn, capability)
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    pub fn list_workspaces(
        &self,
        request: &PageRequest,
    ) -> Result<Page<WorkspaceListItem>, AppError> {
        request.validate()?;
        let cursor = request
            .cursor
            .as_deref()
            .map(|cursor| decode_cursor(cursor, "workspaces"))
            .transpose()?;
        let conn = self.store.connection().lock();
        let (rows, total) = repo::list_workspaces(&conn, cursor, request.limit as usize + 1)?;
        Ok(paginate(
            rows,
            total,
            request.limit,
            "workspaces",
            |row| {
                (
                    row.workspace.path.display_path.clone(),
                    row.workspace.id.clone(),
                )
            },
            |row| row,
        ))
    }

    pub fn list_agents(&self, request: &PageRequest) -> Result<Page<AgentListItem>, AppError> {
        request.validate()?;
        let cursor = request
            .cursor
            .as_deref()
            .map(|cursor| decode_cursor(cursor, "agents"))
            .transpose()?;
        let conn = self.store.connection().lock();
        let (rows, total) = repo::list_agents(&conn, cursor, request.limit as usize + 1)?;
        Ok(paginate(
            rows,
            total,
            request.limit,
            "agents",
            |row| (row.agent.display_name.clone(), row.agent.id.clone()),
            |row| row,
        ))
    }

    pub fn list_sessions(&self, query: &SessionQuery) -> Result<Page<SessionSummary>, AppError> {
        query.validate()?;
        let cursor = match query.cursor.as_deref() {
            Some(cursor) => {
                let (key, id) = decode_cursor(cursor, "sessions")?;
                let created_at_ms = key.parse::<i64>().map_err(|_| {
                    AppError::invalid_argument("cursor", "session cursor timestamp is invalid")
                })?;
                Some((created_at_ms, id))
            }
            None => None,
        };
        let conn = self.store.connection().lock();
        let filter = repo::SessionFilter {
            workspace_id: query.workspace_id.as_deref(),
            agent_id: query.agent_id.as_deref(),
            origin: query.origin,
        };
        let (rows, total) = repo::list_sessions(&conn, &filter, cursor, query.limit as usize + 1)?;
        let ids = rows
            .iter()
            .take(query.limit as usize)
            .map(|row| row.session.id.clone())
            .collect::<Vec<_>>();
        let mut workspace_ids = repo::workspace_ids_for_sessions(&conn, &ids)?;
        Ok(paginate(
            rows,
            total,
            query.limit,
            "sessions",
            |row| (row.created_at_ms.to_string(), row.session.id.clone()),
            |row| {
                let linked = workspace_ids.remove(&row.session.id).unwrap_or_default();
                SessionSummary {
                    session: row.session,
                    primary_workspace_id: row.primary_workspace_id,
                    workspace_ids: linked,
                    message_count: row.message_count,
                    run_count: row.run_count,
                }
            },
        ))
    }

    pub fn get_session(&self, session_id: &str) -> Result<SessionDetail, AppError> {
        require_id("sessionId", session_id)?;
        let conn = self.store.connection().lock();
        let session = repo::get_session(&conn, session_id)?
            .ok_or_else(|| AppError::not_found("session", session_id))?;
        let workspaces = repo::session_workspaces(&conn, session_id)?;
        let runs = repo::runs_for_session(&conn, session_id)?;
        let messages = repo::messages_for_session(&conn, session_id)?;
        Ok(SessionDetail {
            session,
            message_count: messages.len() as u64,
            run_count: runs.len() as u64,
            workspaces,
            runs,
            messages,
        })
    }

    pub fn list_capabilities(&self, agent_id: &str) -> Result<Vec<Capability>, AppError> {
        require_id("agentId", agent_id)?;
        let conn = self.store.connection().lock();
        repo::list_capabilities(&conn, agent_id)
    }

    pub fn list_capabilities_page(
        &self,
        query: &CapabilityQuery,
    ) -> Result<Page<Capability>, AppError> {
        query.validate()?;
        let cursor = query
            .cursor
            .as_deref()
            .map(|cursor| decode_cursor(cursor, "capabilities"))
            .transpose()?;
        let conn = self.store.connection().lock();
        let (rows, total) =
            repo::list_capabilities_page(&conn, &query.agent_id, cursor, query.limit as usize + 1)?;
        Ok(paginate(
            rows,
            total,
            query.limit,
            "capabilities",
            |capability| {
                (
                    capability.name.as_str().to_string(),
                    capability.installation_id.clone().unwrap_or_default(),
                )
            },
            |capability| capability,
        ))
    }

    // -----------------------------------------------------------------------
    // Paths and workspace identity
    // -----------------------------------------------------------------------

    /// Resolves a real Windows path without touching the database. Missing or
    /// inaccessible directories keep `canonicalPath = null` and a status.
    pub fn resolve_workspace_path(
        &self,
        host_id: &str,
        display_path: &str,
    ) -> ResolvedWorkspacePath {
        path::resolve_workspace_path(host_id, display_path)
    }

    /// Registers a real directory as a workspace using its canonical identity.
    /// Re-registering the same `(hostId, canonicalPath)` returns the stored row,
    /// so case aliases and junctions do not create duplicates.
    pub fn ensure_workspace_at_path(
        &self,
        host_id: &str,
        display_path: &str,
    ) -> Result<Workspace, AppError> {
        let resolved = path::resolve_workspace_path(host_id, display_path);
        let candidate = path::workspace_from_resolved(&resolved)?;
        let mut conn = self.store.connection().lock();
        repo::ensure_workspace_by_identity(&mut conn, &candidate)
    }

    // -----------------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------------

    /// Writes a redacted diagnostics document under `<dataRoot>/diagnostics`.
    /// The UI receives the local file path; the file itself contains no paths,
    /// messages, prompts, commands or native session ids.
    pub fn export_diagnostics(&self, profile: &str) -> Result<DiagnosticsExport, AppError> {
        if profile != "dev" && profile != "test" && profile != "prod" {
            return Err(AppError::invalid_argument(
                "profile",
                "must be dev, test or prod",
            ));
        }
        let epoch_ms = now_ms();
        let created_at = iso_timestamp(epoch_ms);
        let (counts, breakdown, journal_mode) = {
            let conn = self.store.connection().lock();
            let counts = repo::store_counts(&conn).map_err(retryable_read)?;
            let breakdown = repo::capability_breakdown(&conn).map_err(retryable_read)?;
            let journal_mode: String = conn
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .map_err(AppError::from)
                .map_err(retryable_read)?;
            (counts, breakdown, journal_mode)
        };

        let mut capabilities: BTreeMap<String, CapabilityCounts> = BTreeMap::new();
        for row in breakdown {
            let entry = capabilities.entry(row.agent_id).or_default();
            match row.status {
                super::domain::CapabilityStatus::Supported => entry.supported += row.count,
                super::domain::CapabilityStatus::Unsupported => entry.unsupported += row.count,
                super::domain::CapabilityStatus::Unverified => entry.unverified += row.count,
            }
            match row.evidence_level {
                super::domain::CapabilityEvidenceLevel::Document => entry.document += row.count,
                super::domain::CapabilityEvidenceLevel::Fixture => entry.fixture += row.count,
                super::domain::CapabilityEvidenceLevel::Live => entry.live += row.count,
            }
        }

        let document = build_diagnostics(
            profile,
            env!("CARGO_PKG_VERSION"),
            self.schema_version(),
            &journal_mode,
            &DiagnosticsCounts {
                hosts: counts.hosts,
                agents: counts.agents,
                installations: counts.installations,
                workspaces: counts.workspaces,
                sessions: counts.sessions,
                runs: counts.runs,
                messages: counts.messages,
                capabilities: counts.capabilities,
                processes: counts.processes,
                run_process_links: counts.run_process_links,
            },
            &capabilities,
            &created_at,
        );
        write_diagnostics(self.store.data_root(), &document, epoch_ms, &created_at)
    }

    pub fn get_process(
        &self,
        identity: &ProcessIdentity,
    ) -> Result<Option<ProcessObservation>, AppError> {
        identity.validate()?;
        let conn = self.store.connection().lock();
        repo::read_process(&conn, identity)
    }

    pub fn links_for_run(&self, run_id: &str) -> Result<Vec<RunProcessLink>, AppError> {
        require_id("runId", run_id)?;
        let conn = self.store.connection().lock();
        repo::links_for_run(&conn, run_id)
    }

    #[cfg(test)]
    pub(crate) fn open_for_test(
        data_root: &Path,
        fault: super::store::MigrationFault,
    ) -> Result<Self, AppError> {
        Ok(Self {
            store: MultiCliStore::open_for_test(data_root, fault)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn execute_raw(&self, sql: &str) -> Result<(), AppError> {
        self.store.execute_raw(sql)
    }

    #[cfg(test)]
    pub(crate) fn lock_for_test(&self) -> MutexGuard<'_, Connection> {
        self.store.connection().lock()
    }
}

fn require_id(field: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() {
        return Err(AppError::invalid_argument(field, "must not be empty"));
    }
    Ok(())
}

fn retryable_read(error: AppError) -> AppError {
    if matches!(
        error.code,
        super::error::ErrorCode::Database
            | super::error::ErrorCode::Io
            | super::error::ErrorCode::Timeout
    ) {
        error.with_retryable(true)
    } else {
        error
    }
}
