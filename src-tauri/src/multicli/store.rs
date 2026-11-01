//! Isolated MultiCLI SQLite store with versioned, transactional migrations.
//!
//! The database lives under an explicitly supplied MultiCLI data root and is
//! always named `multicli.db`. The upstream `app.db` is never opened, migrated
//! or renamed. Every schema upgrade takes a consistent `VACUUM INTO` backup
//! before touching the file and rolls back its transaction on failure.

use super::error::{AppError, ErrorCode};
use parking_lot::Mutex;
use rusqlite::{params, Connection, TransactionBehavior};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const SCHEMA_VERSION: i64 = 2;

const DB_FILE_NAME: &str = "multicli.db";

struct Migration {
    version: i64,
    description: &'static str,
    /// Each element is executed as its own batch. A test-only fault can stop
    /// between elements, which proves the transaction rolls back partial DDL.
    statements: &'static [&'static str],
}

const V1_CORE: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations(
    version INTEGER PRIMARY KEY,
    description TEXT NOT NULL,
    applied_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS hosts(
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('local-windows')),
    path_style TEXT NOT NULL CHECK(path_style IN ('windows')),
    created_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS agents(
    id TEXT PRIMARY KEY,
    vendor TEXT NOT NULL CHECK(length(trim(vendor)) > 0),
    display_name TEXT NOT NULL CHECK(length(trim(display_name)) > 0),
    created_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS agent_installations(
    id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    executable_display_path TEXT NOT NULL CHECK(length(trim(executable_display_path)) > 0),
    executable_canonical_path TEXT CHECK(
        executable_canonical_path IS NULL OR length(trim(executable_canonical_path)) > 0
    ),
    version TEXT CHECK(version IS NULL OR length(trim(version)) > 0),
    config_profile_id TEXT NOT NULL CHECK(length(trim(config_profile_id)) > 0),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(host_id, agent_id, config_profile_id)
) STRICT;

CREATE TABLE IF NOT EXISTS workspaces(
    id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    display_path TEXT NOT NULL CHECK(length(trim(display_path)) > 0),
    canonical_path TEXT CHECK(
        canonical_path IS NULL OR length(trim(canonical_path)) > 0
    ),
    repo_group_id TEXT CHECK(repo_group_id IS NULL OR length(trim(repo_group_id)) > 0),
    worktree_id TEXT CHECK(worktree_id IS NULL OR length(trim(worktree_id)) > 0),
    created_at_ms INTEGER NOT NULL
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS ux_workspaces_host_canonical
    ON workspaces(host_id, canonical_path)
    WHERE canonical_path IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_workspaces_order
    ON workspaces(display_path, id);

CREATE TABLE IF NOT EXISTS sessions(
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    source_namespace TEXT NOT NULL CHECK(length(trim(source_namespace)) > 0),
    native_id TEXT CHECK(native_id IS NULL OR length(trim(native_id)) > 0),
    origin TEXT NOT NULL CHECK(origin IN ('managed', 'external-observed', 'history-only')),
    title TEXT NOT NULL DEFAULT '',
    created_at_ms INTEGER NOT NULL
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS ux_sessions_native
    ON sessions(source_namespace, native_id)
    WHERE native_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_sessions_agent
    ON sessions(agent_id, created_at_ms DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_sessions_order
    ON sessions(created_at_ms DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_sessions_origin
    ON sessions(origin, created_at_ms DESC, id DESC);

CREATE TABLE IF NOT EXISTS session_workspaces(
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK(role IN ('primary', 'related')),
    evidence TEXT NOT NULL CHECK(length(trim(evidence)) > 0),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(session_id, workspace_id)
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS ux_session_workspaces_primary
    ON session_workspaces(session_id)
    WHERE role = 'primary';
CREATE INDEX IF NOT EXISTS ix_session_workspaces_workspace
    ON session_workspaces(workspace_id, session_id);

CREATE TABLE IF NOT EXISTS runs(
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    installation_id TEXT REFERENCES agent_installations(id) ON DELETE SET NULL,
    transport TEXT NOT NULL CHECK(transport IN ('native', 'acp', 'pty')),
    ownership TEXT NOT NULL CHECK(ownership IN ('managed', 'external-observed')),
    started_at TEXT CHECK(started_at IS NULL OR length(trim(started_at)) > 0),
    ended_at TEXT CHECK(ended_at IS NULL OR length(trim(ended_at)) > 0),
    execution_status TEXT NOT NULL CHECK(
        execution_status IN ('starting', 'running', 'stopping', 'exited', 'failed', 'unknown')
    ),
    interaction_status TEXT NOT NULL CHECK(
        interaction_status IN ('working', 'awaiting-permission', 'awaiting-input', 'idle', 'unknown')
    ),
    evidence_level TEXT NOT NULL CHECK(evidence_level IN ('observed', 'inferred')),
    evidence TEXT CHECK(evidence IS NULL OR length(trim(evidence)) > 0),
    created_at_ms INTEGER NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS ix_runs_session
    ON runs(session_id, created_at_ms, id);

CREATE TABLE IF NOT EXISTS processes(
    host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    pid INTEGER NOT NULL CHECK(pid >= 0),
    creation_time TEXT NOT NULL CHECK(length(trim(creation_time)) > 0),
    executable_display_path TEXT CHECK(
        executable_display_path IS NULL OR length(trim(executable_display_path)) > 0
    ),
    executable_canonical_path TEXT CHECK(
        executable_canonical_path IS NULL OR length(trim(executable_canonical_path)) > 0
    ),
    redacted_command TEXT,
    parent_host_id TEXT,
    parent_pid INTEGER CHECK(parent_pid IS NULL OR parent_pid >= 0),
    parent_creation_time TEXT CHECK(
        parent_creation_time IS NULL OR length(trim(parent_creation_time)) > 0
    ),
    observed_at TEXT NOT NULL CHECK(length(trim(observed_at)) > 0),
    first_seen_at_ms INTEGER NOT NULL,
    PRIMARY KEY(host_id, pid, creation_time),
    CHECK(
        (parent_host_id IS NULL AND parent_pid IS NULL AND parent_creation_time IS NULL)
        OR
        (parent_host_id IS NOT NULL AND parent_pid IS NOT NULL AND parent_creation_time IS NOT NULL)
    )
) STRICT;

CREATE TABLE IF NOT EXISTS run_process_links(
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    host_id TEXT NOT NULL,
    pid INTEGER NOT NULL CHECK(pid >= 0),
    creation_time TEXT NOT NULL,
    link_kind TEXT NOT NULL CHECK(link_kind IN ('owned', 'observed', 'inferred')),
    evidence TEXT NOT NULL CHECK(length(trim(evidence)) > 0),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(run_id, host_id, pid, creation_time),
    FOREIGN KEY(host_id, pid, creation_time)
        REFERENCES processes(host_id, pid, creation_time) ON DELETE CASCADE
) STRICT;

CREATE TABLE IF NOT EXISTS session_relations(
    parent_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    child_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('os-parent', 'explicit-delegation', 'inferred')),
    confidence REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
    evidence TEXT NOT NULL CHECK(length(trim(evidence)) > 0),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(parent_session_id, child_session_id, kind),
    CHECK(parent_session_id <> child_session_id)
) STRICT;

CREATE TABLE IF NOT EXISTS capabilities(
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    installation_id TEXT REFERENCES agent_installations(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK(name IN (
        'installedDetection', 'processDetection', 'historyRead', 'historySearch',
        'resume', 'structuredChat', 'permissionFlow', 'pty', 'quotaRead'
    )),
    status TEXT NOT NULL CHECK(status IN ('supported', 'unsupported', 'unverified')),
    version_range TEXT CHECK(version_range IS NULL OR length(trim(version_range)) > 0),
    evidence_level TEXT NOT NULL CHECK(evidence_level IN ('document', 'fixture', 'live')),
    source TEXT NOT NULL CHECK(length(trim(source)) > 0),
    verified_at TEXT CHECK(verified_at IS NULL OR length(trim(verified_at)) > 0),
    reason TEXT CHECK(reason IS NULL OR length(trim(reason)) > 0),
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS ux_capabilities_scope
    ON capabilities(agent_id, name, COALESCE(installation_id, ''));

CREATE TABLE IF NOT EXISTS messages(
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id TEXT REFERENCES runs(id) ON DELETE SET NULL,
    native_event_id TEXT CHECK(
        native_event_id IS NULL OR length(trim(native_event_id)) > 0
    ),
    source_key TEXT NOT NULL CHECK(length(trim(source_key)) > 0),
    seq INTEGER NOT NULL CHECK(seq >= 0),
    kind TEXT NOT NULL CHECK(length(trim(kind)) > 0),
    content_json TEXT NOT NULL CHECK(json_valid(content_json) AND json_type(content_json) = 'object'),
    source_ref TEXT CHECK(source_ref IS NULL OR length(trim(source_ref)) > 0),
    observed_at TEXT NOT NULL CHECK(length(trim(observed_at)) > 0),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(session_id, source_key)
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS ux_messages_native_event
    ON messages(session_id, native_event_id)
    WHERE native_event_id IS NOT NULL;

-- User-owned data. Derived indexes may be rebuilt at any time, but these
-- tables are never dropped by a migration or an index pass.
CREATE TABLE IF NOT EXISTS user_session_notes(
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    note TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS user_session_tags(
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    tag TEXT NOT NULL CHECK(length(trim(tag)) > 0),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(session_id, tag)
) STRICT;
"#;

const V2_MESSAGE_ORDER_INDEX: &str = r#"
CREATE INDEX IF NOT EXISTS ix_messages_session_order
    ON messages(session_id, observed_at, seq, id);
"#;

const V2_RUN_MESSAGE_INDEX: &str = r#"
CREATE INDEX IF NOT EXISTS ix_messages_run
    ON messages(run_id, seq, id);
"#;

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        description: "core domain schema and user-owned notes/tags",
        statements: &[V1_CORE],
    },
    Migration {
        version: 2,
        description: "message access indexes for session detail and run tracing",
        statements: &[V2_MESSAGE_ORDER_INDEX, V2_RUN_MESSAGE_INDEX],
    },
];

#[derive(Debug, Clone, Default, Copy)]
pub(crate) struct MigrationFault {
    /// Stop successfully after this version. Used to construct a real v1
    /// database before exercising a v1 -> v2 failure.
    pub(crate) stop_after_version: Option<i64>,
    /// (migration version, zero-based statement index) that must fail before
    /// executing. The first statement of that migration has already run, so
    /// the rollback covers partial DDL.
    pub(crate) fail_at: Option<(i64, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupInfo {
    pub path: PathBuf,
    pub manifest_path: PathBuf,
    pub from_version: i64,
    pub target_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub from_version: i64,
    pub to_version: i64,
    pub backup: Option<BackupInfo>,
}

pub struct MultiCliStore {
    data_root: PathBuf,
    db_path: PathBuf,
    conn: Mutex<Connection>,
    schema_version: i64,
    migration_report: MigrationReport,
}

impl MultiCliStore {
    pub fn open(data_root: impl AsRef<Path>) -> Result<Self, AppError> {
        Self::open_inner(data_root.as_ref(), MigrationFault::default())
    }

    #[cfg(test)]
    pub(crate) fn open_for_test(data_root: &Path, fault: MigrationFault) -> Result<Self, AppError> {
        Self::open_inner(data_root, fault)
    }

    fn open_inner(data_root: &Path, fault: MigrationFault) -> Result<Self, AppError> {
        let data_root = validated_root(data_root)?;
        std::fs::create_dir_all(&data_root)?;
        let db_path = data_root.join(DB_FILE_NAME);
        let mut conn = Connection::open(&db_path)?;
        // Version inspection must happen before WAL or any other persistent
        // PRAGMA. A database written by a newer build is rejected without
        // changing its journal mode, schema or bytes.
        let existing_version = current_version(&conn)?;
        if existing_version > SCHEMA_VERSION {
            return Err(schema_too_new(existing_version));
        }
        if !table_exists(&conn, "schema_migrations")? && database_has_tables(&conn)? {
            return Err(AppError::new(
                ErrorCode::Database,
                "refusing to migrate a nonempty database without MultiCLI schema metadata",
            )
            .with_details(json!({ "stage": "open", "reason": "unrecognizedDatabase" })));
        }
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let migration_report = migrate(&mut conn, &data_root, fault)?;
        Ok(Self {
            data_root,
            db_path,
            conn: Mutex::new(conn),
            schema_version: migration_report.to_version,
            migration_report,
        })
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn schema_version(&self) -> i64 {
        self.schema_version
    }

    pub fn migration_report(&self) -> &MigrationReport {
        &self.migration_report
    }

    pub fn journal_mode(&self) -> Result<String, AppError> {
        let conn = self.conn.lock();
        Ok(conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?)
    }

    pub fn foreign_keys_enabled(&self) -> Result<bool, AppError> {
        let conn = self.conn.lock();
        let enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        Ok(enabled != 0)
    }

    pub(crate) fn connection(&self) -> &Mutex<Connection> {
        &self.conn
    }

    #[cfg(test)]
    pub(crate) fn execute_raw(&self, sql: &str) -> Result<(), AppError> {
        let conn = self.conn.lock();
        conn.execute_batch(sql).map_err(AppError::from)
    }
}

fn validated_root(path: &Path) -> Result<PathBuf, AppError> {
    if !path.is_absolute() {
        return Err(AppError::invalid_argument(
            "dataRoot",
            "must be an absolute MultiCLI data root",
        ));
    }
    reject_legacy_components(path)?;
    // Resolve existing ancestors so a junction/reparse point cannot smuggle
    // the real target into .ccgui-next after a purely lexical check.
    let resolved = resolve_existing_ancestor(path)?;
    reject_legacy_components(&resolved)?;
    if resolved.exists() && !resolved.is_dir() {
        return Err(AppError::invalid_argument(
            "dataRoot",
            "existing path is not a directory",
        ));
    }
    Ok(resolved)
}

fn reject_legacy_components(path: &Path) -> Result<(), AppError> {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            if name.to_string_lossy().eq_ignore_ascii_case(".ccgui-next") {
                return Err(AppError::invalid_argument(
                    "dataRoot",
                    "must not point at the upstream .ccgui-next data directory",
                )
                .with_details(serde_json::json!({
                    "stage": "validateDataRoot",
                    "path": path.display().to_string(),
                })));
            }
        }
    }
    Ok(())
}

fn resolve_existing_ancestor(path: &Path) -> Result<PathBuf, AppError> {
    let mut existing = path.to_path_buf();
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            AppError::invalid_argument("dataRoot", "has no existing parent directory")
        })?;
        suffix.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| {
                AppError::invalid_argument("dataRoot", "has no existing parent directory")
            })?
            .to_path_buf();
    }
    let canonical = std::fs::canonicalize(&existing).map_err(|error| {
        AppError::new(
            ErrorCode::Io,
            format!("failed to resolve data root target: {error}"),
        )
        .with_details(serde_json::json!({
            "stage": "validateDataRoot",
            "path": existing.display().to_string(),
        }))
    })?;
    if !canonical.is_dir() {
        return Err(
            AppError::invalid_argument("dataRoot", "existing ancestor is not a directory")
                .with_details(serde_json::json!({
                    "stage": "validateDataRoot",
                    "path": canonical.display().to_string(),
                })),
        );
    }
    // Expose the normalized path (no Windows `\\?\` verbatim prefix) so the
    // diagnostics response and UI show a path the user can actually paste.
    let mut resolved = PathBuf::from(super::path::canonical_text(&canonical));
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn schema_too_new(version: i64) -> AppError {
    AppError::new(
        ErrorCode::Database,
        format!(
            "database schema version {version} is newer than supported version {SCHEMA_VERSION}"
        ),
    )
    .with_details(json!({
        "stage": "open",
        "databaseVersion": version,
        "supportedVersion": SCHEMA_VERSION,
    }))
}

fn current_version(conn: &Connection) -> Result<i64, AppError> {
    if !table_exists(conn, "schema_migrations")? {
        return Ok(0);
    }
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?)
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool, AppError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn database_has_tables(conn: &Connection) -> Result<bool, AppError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn migrate(
    conn: &mut Connection,
    data_root: &Path,
    fault: MigrationFault,
) -> Result<MigrationReport, AppError> {
    let from_version = current_version(conn)?;
    if from_version > SCHEMA_VERSION {
        return Err(schema_too_new(from_version));
    }

    let requested_target = fault.stop_after_version.unwrap_or(SCHEMA_VERSION);
    let target_version = requested_target.clamp(0, SCHEMA_VERSION).max(from_version);

    let backup = if from_version < target_version && database_has_tables(conn)? {
        Some(create_backup(
            conn,
            data_root,
            from_version,
            target_version,
        )?)
    } else {
        None
    };

    let mut applied_version = from_version;
    for migration in MIGRATIONS
        .iter()
        .filter(|m| m.version > from_version && m.version <= target_version)
    {
        if let Err(error) = apply_migration(conn, migration, fault) {
            let failed_version = migration.version;
            let cause = error.details.clone();
            return Err(AppError::new(
                error.code,
                format!(
                    "schema migration to version {failed_version} failed; database left at version {applied_version}: {}",
                    error.message
                ),
            )
            .with_details(json!({
                "stage": "migration",
                "fromVersion": from_version,
                "targetVersion": target_version,
                "failedVersion": failed_version,
                "databaseVersion": applied_version,
                "backupPath": backup.as_ref().map(|b| b.path.display().to_string()),
                "cause": cause,
            })));
        }
        applied_version = migration.version;
        if fault.stop_after_version == Some(migration.version) {
            break;
        }
    }

    Ok(MigrationReport {
        from_version,
        to_version: applied_version,
        backup,
    })
}

fn apply_migration(
    conn: &mut Connection,
    migration: &Migration,
    fault: MigrationFault,
) -> Result<(), AppError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for (index, statement) in migration.statements.iter().enumerate() {
        if fault.fail_at == Some((migration.version, index)) {
            return Err(AppError::new(
                ErrorCode::Database,
                format!(
                    "injected migration failure at version {} statement {} (test-only fault)",
                    migration.version,
                    index + 1
                ),
            ));
        }
        tx.execute_batch(statement)?;
    }
    tx.execute(
        "INSERT INTO schema_migrations(version, description, applied_at_ms) VALUES (?1, ?2, ?3)",
        params![migration.version, migration.description, now_ms()],
    )?;
    tx.commit()?;
    Ok(())
}

fn create_backup(
    conn: &Connection,
    data_root: &Path,
    from_version: i64,
    target_version: i64,
) -> Result<BackupInfo, AppError> {
    let backup_dir = data_root.join("backups");
    std::fs::create_dir_all(&backup_dir)?;
    let stamp = now_ms();
    let unique = Uuid::new_v4().simple().to_string();
    let file_name = format!("multicli-v{from_version}-to-v{target_version}-{stamp}-{unique}.db");
    let path = backup_dir.join(&file_name);
    let path_text = path.to_string_lossy().to_string();
    // VACUUM INTO writes a consistent snapshot and cannot run inside a
    // transaction, which is why it happens before BEGIN IMMEDIATE.
    conn.execute("VACUUM INTO ?1", params![path_text])?;

    let manifest_path = path.with_extension("json");
    let manifest = json!({
        "format": "multicli-sqlite-backup-v1",
        "databaseFile": DB_FILE_NAME,
        "backupFile": file_name,
        "fromVersion": from_version,
        "targetVersion": target_version,
        "createdAtMs": stamp,
    });
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;

    Ok(BackupInfo {
        path,
        manifest_path,
        from_version,
        target_version,
    })
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_and_upstream_roots_are_rejected() {
        assert_eq!(
            validated_root(Path::new("relative/root")).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        let upstream = std::env::temp_dir().join(".ccgui-next");
        assert_eq!(
            validated_root(&upstream).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
}
