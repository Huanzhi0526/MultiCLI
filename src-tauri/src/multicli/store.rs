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

pub const SCHEMA_VERSION: i64 = 7;

const DB_FILE_NAME: &str = "multicli.db";

struct Migration {
    version: i64,
    description: &'static str,
    /// Each element is executed as its own batch. A test-only fault can stop
    /// between elements, which proves the transaction rolls back partial DDL.
    statements: &'static [&'static str],
    /// A table rebuild must run with `foreign_keys = OFF` so `DROP TABLE`
    /// cannot fire `ON DELETE SET NULL` on the child rows it is re-pointing.
    requires_foreign_keys_off: bool,
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

/// Schema v3 — installation identity plus the discovery process state table.
///
/// The old unique key `(host_id, agent_id, config_profile_id)` could not
/// express two executables of one product under one profile. The rebuild adds
/// an explicit `executable_identity` column and extends the unique key with
/// it. Legacy rows without a canonical path keep an explicitly *unresolved*
/// display identity; no path is guessed. Existing ids, and therefore every
/// Run/Capability foreign key, are copied unchanged.
const V3_INSTALLATION_IDENTITY: &str = r#"
CREATE TABLE agent_installations_v3(
    id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    executable_display_path TEXT NOT NULL CHECK(length(trim(executable_display_path)) > 0),
    executable_canonical_path TEXT CHECK(
        executable_canonical_path IS NULL OR length(trim(executable_canonical_path)) > 0
    ),
    executable_identity TEXT NOT NULL CHECK(length(trim(executable_identity)) > 0),
    executable_identity_kind TEXT NOT NULL CHECK(
        executable_identity_kind IN ('canonical', 'unresolved-display', 'package', 'shim')
    ),
    version TEXT CHECK(version IS NULL OR length(trim(version)) > 0),
    config_profile_id TEXT NOT NULL CHECK(length(trim(config_profile_id)) > 0),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(host_id, agent_id, config_profile_id, executable_identity)
) STRICT;

INSERT INTO agent_installations_v3(
    id, host_id, agent_id, executable_display_path, executable_canonical_path,
    executable_identity, executable_identity_kind, version, config_profile_id, created_at_ms
)
SELECT id, host_id, agent_id, executable_display_path, executable_canonical_path,
    CASE WHEN executable_canonical_path IS NOT NULL
         THEN 'canonical:' || lower(replace(executable_canonical_path, '/', '\'))
         ELSE 'unresolved-display:' || lower(replace(executable_display_path, '/', '\'))
    END,
    CASE WHEN executable_canonical_path IS NOT NULL
         THEN 'canonical' ELSE 'unresolved-display'
    END,
    version, config_profile_id, created_at_ms
FROM agent_installations;

DROP TABLE agent_installations;

ALTER TABLE agent_installations_v3 RENAME TO agent_installations;
"#;

const V3_DISCOVERY_STATE: &str = r#"
CREATE TABLE discovery_processes(
    host_id TEXT NOT NULL,
    pid INTEGER NOT NULL CHECK(pid >= 0),
    creation_time TEXT NOT NULL CHECK(length(trim(creation_time)) > 0),
    agent_id TEXT,
    installation_id TEXT,
    product TEXT,
    entry_kind TEXT,
    recognition TEXT CHECK(recognition IS NULL OR recognition IN ('confirmed', 'likely')),
    evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
    state TEXT NOT NULL CHECK(state IN ('alive', 'exited', 'unknown')),
    freshness TEXT NOT NULL CHECK(freshness IN ('fresh', 'stale')),
    state_reason TEXT,
    executable_display_path TEXT,
    executable_canonical_path TEXT,
    redacted_command TEXT,
    parent_host_id TEXT,
    parent_pid INTEGER CHECK(parent_pid IS NULL OR parent_pid >= 0),
    parent_creation_time TEXT,
    parent_note TEXT,
    source_program TEXT,
    workspace_id TEXT,
    workspace_evidence TEXT,
    first_seen_at TEXT NOT NULL CHECK(length(trim(first_seen_at)) > 0),
    last_seen_at TEXT NOT NULL CHECK(length(trim(last_seen_at)) > 0),
    last_seen_generation INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY(host_id, pid, creation_time),
    CHECK(
        (parent_host_id IS NULL AND parent_pid IS NULL AND parent_creation_time IS NULL)
        OR
        (parent_host_id IS NOT NULL AND parent_pid IS NOT NULL AND parent_creation_time IS NOT NULL)
    )
) STRICT;

CREATE INDEX ix_discovery_processes_order
    ON discovery_processes(agent_id, last_seen_at DESC, pid);
"#;

/// Schema v4 — read-only history sources, per-file cursors, derived-index
/// provenance and the FTS5 search index.
///
/// Nothing here stores a copy of a CLI configuration file. A source row records
/// the user's *choice* of root plus its canonical identity; a cursor records how
/// far one file was imported and the file identity that proves it is still the
/// same file. `history_message_origins` is what lets a rebuild remove only the
/// rows a single source produced while leaving every user-owned note/tag and
/// every other source untouched.
///
/// The FTS table is a normal (not external-content) FTS5 table so a row can be
/// deleted by `message_id` without a trigger, and a rebuild cannot leave stale
/// shadows behind. `trigram` is required for Chinese substring search; short
/// queries (1–2 characters) cannot be answered by a trigram index and fall back
/// to an escaped `LIKE` scan, which is a deliberate, documented cost.
const V4_HISTORY_INDEX: &str = r#"
CREATE TABLE IF NOT EXISTS history_sources(
    id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    product TEXT NOT NULL CHECK(length(trim(product)) > 0),
    installation_id TEXT REFERENCES agent_installations(id) ON DELETE SET NULL,
    config_profile_id TEXT NOT NULL CHECK(length(trim(config_profile_id)) > 0),
    root_display_path TEXT NOT NULL CHECK(length(trim(root_display_path)) > 0),
    root_canonical_path TEXT NOT NULL CHECK(length(trim(root_canonical_path)) > 0),
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    parser_version INTEGER NOT NULL CHECK(parser_version > 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(host_id, agent_id, config_profile_id, root_canonical_path)
) STRICT;

CREATE TABLE IF NOT EXISTS history_index_state(
    source_id TEXT PRIMARY KEY REFERENCES history_sources(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL CHECK(status IN (
        'idle', 'indexing', 'ok', 'partial', 'error', 'cancelled'
    )),
    in_flight INTEGER NOT NULL DEFAULT 0 CHECK(in_flight IN (0, 1)),
    last_attempt_at TEXT CHECK(last_attempt_at IS NULL OR length(trim(last_attempt_at)) > 0),
    last_success_at TEXT CHECK(last_success_at IS NULL OR length(trim(last_success_at)) > 0),
    last_error_json TEXT CHECK(last_error_json IS NULL OR json_valid(last_error_json)),
    files_total INTEGER NOT NULL DEFAULT 0 CHECK(files_total >= 0),
    files_done INTEGER NOT NULL DEFAULT 0 CHECK(files_done >= 0),
    files_skipped INTEGER NOT NULL DEFAULT 0 CHECK(files_skipped >= 0),
    files_failed INTEGER NOT NULL DEFAULT 0 CHECK(files_failed >= 0),
    messages_indexed INTEGER NOT NULL DEFAULT 0 CHECK(messages_indexed >= 0),
    bytes_read INTEGER NOT NULL DEFAULT 0 CHECK(bytes_read >= 0),
    complete INTEGER NOT NULL DEFAULT 0 CHECK(complete IN (0, 1)),
    note TEXT,
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS history_cursors(
    source_id TEXT NOT NULL REFERENCES history_sources(id) ON DELETE CASCADE,
    file_identity TEXT NOT NULL CHECK(length(trim(file_identity)) > 0),
    display_path TEXT NOT NULL CHECK(length(trim(display_path)) > 0),
    canonical_path TEXT NOT NULL CHECK(length(trim(canonical_path)) > 0),
    size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
    mtime_ms INTEGER NOT NULL,
    head_hash TEXT NOT NULL CHECK(length(trim(head_hash)) > 0),
    anchor_hash TEXT NOT NULL CHECK(length(trim(anchor_hash)) > 0),
    committed_offset INTEGER NOT NULL CHECK(committed_offset >= 0),
    tail_residue TEXT NOT NULL DEFAULT '',
    tail_residue_truncated INTEGER NOT NULL DEFAULT 0 CHECK(tail_residue_truncated IN (0, 1)),
    parser_version INTEGER NOT NULL CHECK(parser_version > 0),
    error_count INTEGER NOT NULL DEFAULT 0 CHECK(error_count >= 0),
    last_error_json TEXT CHECK(last_error_json IS NULL OR json_valid(last_error_json)),
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY(source_id, file_identity)
) STRICT;

CREATE INDEX IF NOT EXISTS ix_history_cursors_path
    ON history_cursors(source_id, canonical_path);

CREATE TABLE IF NOT EXISTS history_message_origins(
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES history_sources(id) ON DELETE CASCADE,
    file_identity TEXT NOT NULL CHECK(length(trim(file_identity)) > 0),
    byte_offset INTEGER NOT NULL CHECK(byte_offset >= 0),
    line_index INTEGER NOT NULL CHECK(line_index >= 0),
    parser_version INTEGER NOT NULL CHECK(parser_version > 0),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(message_id, source_id)
) STRICT;

CREATE INDEX IF NOT EXISTS ix_history_origins_file
    ON history_message_origins(source_id, file_identity);

CREATE TABLE IF NOT EXISTS history_session_facts(
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES history_sources(id) ON DELETE CASCADE,
    native_id TEXT NOT NULL CHECK(length(trim(native_id)) > 0),
    title_source TEXT NOT NULL CHECK(title_source IN ('native-title', 'first-user-message', 'none')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY(session_id, source_id)
) STRICT;

CREATE INDEX IF NOT EXISTS ix_history_session_facts_native
    ON history_session_facts(source_id, native_id);
"#;

const V4_HISTORY_FTS: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS history_message_fts USING fts5(
    message_id UNINDEXED,
    session_id UNINDEXED,
    body,
    observed_at UNINDEXED,
    seq UNINDEXED,
    session_created_at UNINDEXED,
    tokenize = 'trigram'
);
"#;

/// Schema v5 — provenance per contributing file, resumable read state and
/// content-verification blocks.
///
/// Three defects motivated it:
///
/// 1. `history_message_origins` was keyed by `(message_id, source_id)`, so two
///    files of the *same* source that carried the same event overwrote each
///    other; truncating the last one deleted a message the other still
///    provided. The primary key now includes `file_identity` and the purge only
///    removes a message when no origin remains.
/// 2. A cursor only knew a byte offset, not whether the file had actually been
///    read to the end, which parser metadata had been derived from its prefix,
///    or how many lines had been consumed. A pass that stopped on its byte
///    budget therefore looked "unchanged" forever, and an append restarted the
///    line counter so fallback event ids collided. The four added columns make
///    that state explicit and recoverable.
/// 3. Size + mtime + a 4 KiB prefix/anchor hash cannot see a middle rewrite
///    that preserves both. Per-block hashes plus a bounded rotating
///    verification pass give eventual full coverage without re-reading every
///    file every pass.
///
/// Existing v4 rows are migrated in place. Their old cursors have no parser
/// checkpoint, and the importer deliberately treats that as "resume state
/// unknown" and re-reads the file once instead of guessing a line counter.
const V5_HISTORY_INTEGRITY: &str = r#"
CREATE TABLE history_message_origins_v5(
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES history_sources(id) ON DELETE CASCADE,
    file_identity TEXT NOT NULL CHECK(length(trim(file_identity)) > 0),
    byte_offset INTEGER NOT NULL CHECK(byte_offset >= 0),
    line_index INTEGER NOT NULL CHECK(line_index >= 0),
    parser_version INTEGER NOT NULL CHECK(parser_version > 0),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(message_id, source_id, file_identity)
) STRICT;

INSERT INTO history_message_origins_v5(
    message_id, source_id, file_identity, byte_offset, line_index, parser_version, created_at_ms
)
SELECT message_id, source_id, file_identity, byte_offset, line_index, parser_version, created_at_ms
FROM history_message_origins;

DROP TABLE history_message_origins;

ALTER TABLE history_message_origins_v5 RENAME TO history_message_origins;

CREATE INDEX IF NOT EXISTS ix_history_origins_file
    ON history_message_origins(source_id, file_identity);

CREATE INDEX IF NOT EXISTS ix_history_origins_message
    ON history_message_origins(message_id);

-- Search pushes the workspace filter down as `session_id IN (...)`, which needs
-- a workspace-first index on the link table (the primary key is session-first).
CREATE INDEX IF NOT EXISTS ix_session_workspaces_workspace
    ON session_workspaces(workspace_id, session_id);

-- The source filter runs `EXISTS (... WHERE message_id = ? AND source_id = ?)`
-- per FTS row, so the index must lead with source_id and carry message_id.
CREATE INDEX IF NOT EXISTS ix_history_origins_source_message
    ON history_message_origins(source_id, message_id);

ALTER TABLE history_cursors ADD COLUMN lines_committed INTEGER NOT NULL DEFAULT 0
    CHECK(lines_committed >= 0);

ALTER TABLE history_cursors ADD COLUMN read_complete INTEGER NOT NULL DEFAULT 0
    CHECK(read_complete IN (0, 1));

ALTER TABLE history_cursors ADD COLUMN parser_state_json TEXT
    CHECK(parser_state_json IS NULL OR json_valid(parser_state_json));

ALTER TABLE history_cursors ADD COLUMN verify_offset INTEGER NOT NULL DEFAULT 0
    CHECK(verify_offset >= 0);

-- When the last full verification cycle finished. Without it the 8 MiB budget
-- was spent on the same first files every pass and the tail of a large corpus
-- never got verified.
ALTER TABLE history_cursors ADD COLUMN verified_at_ms INTEGER NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS history_file_blocks(
    source_id TEXT NOT NULL REFERENCES history_sources(id) ON DELETE CASCADE,
    file_identity TEXT NOT NULL CHECK(length(trim(file_identity)) > 0),
    block_index INTEGER NOT NULL CHECK(block_index >= 0),
    hash TEXT NOT NULL CHECK(length(trim(hash)) > 0),
    bytes INTEGER NOT NULL CHECK(bytes >= 0),
    PRIMARY KEY(source_id, file_identity, block_index)
) STRICT;

CREATE TABLE IF NOT EXISTS history_meta(
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

INSERT INTO history_meta(key, value) VALUES ('purgeEpoch', '0')
ON CONFLICT(key) DO NOTHING;
"#;

/// Block tables become layered.
///
/// v5 kept one hash per 256 KiB of *decoded* content. That is the right unit
/// for a plain transcript, but a compressed transcript cannot be checked
/// window by window in decoded space without decoding its whole prefix for
/// every window, so a large `.zst` could only be verified by decoding all of
/// it or not at all. The same table now also stores hashes of the *physical*
/// bytes, which restores the bounded window model for compressed files.
///
/// The rewrite is a table rebuild because the primary key changes. Existing
/// rows are copied as `logical`: they remain the baseline for every plain
/// transcript and are simply unused by the compressed path, which builds its
/// physical baseline from the bytes it has just decoded.
const V6_BLOCK_LAYERS: &str = r#"
CREATE TABLE history_file_blocks_v6(
    source_id TEXT NOT NULL REFERENCES history_sources(id) ON DELETE CASCADE,
    file_identity TEXT NOT NULL CHECK(length(trim(file_identity)) > 0),
    layer TEXT NOT NULL CHECK(layer IN ('logical', 'physical')),
    block_index INTEGER NOT NULL CHECK(block_index >= 0),
    hash TEXT NOT NULL CHECK(length(trim(hash)) > 0),
    bytes INTEGER NOT NULL CHECK(bytes >= 0),
    PRIMARY KEY(source_id, file_identity, layer, block_index)
) STRICT;

INSERT INTO history_file_blocks_v6(
    source_id, file_identity, layer, block_index, hash, bytes
)
SELECT source_id, file_identity, 'logical', block_index, hash, bytes
FROM history_file_blocks;

DROP TABLE history_file_blocks;

ALTER TABLE history_file_blocks_v6 RENAME TO history_file_blocks;
"#;

/// Schema v7 — account profiles, one quota pool per confirmed account, usage
/// snapshots/samples, provider backoff state and user-defined CLIs.
///
/// No existing table is touched: every S01–S05 row keeps its id, and a Run or
/// Session foreign key still points at the same record. The migration is
/// additive on purpose, so a rollback only has to drop the new tables (the
/// store's migration transaction already does that automatically).
///
/// Three constraints encode the S06 rules in the database itself rather than
/// only in Rust:
///
/// - `usage_accounts` refuses `confirmed` without a fingerprint;
/// - `usage_pools.pool_key` is unique, so two sessions of one confirmed
///   account cannot create two allowance rows;
/// - `usage_samples` keeps `estimated` explicit and requires a valid percentile
///   range, so a computed cost can never be stored as a bill.
const V7_USAGE: &str = r#"
CREATE TABLE usage_accounts(
    id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    product TEXT NOT NULL CHECK(length(trim(product)) > 0),
    vendor TEXT NOT NULL CHECK(length(trim(vendor)) > 0),
    label TEXT NOT NULL CHECK(length(trim(label)) > 0),
    identity_status TEXT NOT NULL CHECK(
        identity_status IN ('confirmed', 'unconfirmed', 'unknown')
    ),
    identity_fingerprint TEXT CHECK(
        identity_fingerprint IS NULL OR length(trim(identity_fingerprint)) > 0
    ),
    plan TEXT CHECK(plan IS NULL OR length(trim(plan)) > 0),
    source TEXT NOT NULL CHECK(length(trim(source)) > 0),
    observed_at TEXT NOT NULL CHECK(length(trim(observed_at)) > 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK(identity_status <> 'confirmed' OR identity_fingerprint IS NOT NULL)
) STRICT;

CREATE UNIQUE INDEX ux_usage_accounts_identity
    ON usage_accounts(host_id, agent_id, identity_fingerprint)
    WHERE identity_fingerprint IS NOT NULL;

CREATE INDEX ix_usage_accounts_agent
    ON usage_accounts(host_id, agent_id, updated_at_ms DESC);

CREATE TABLE usage_pools(
    id TEXT PRIMARY KEY,
    pool_key TEXT NOT NULL CHECK(length(trim(pool_key)) > 0),
    host_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    product TEXT NOT NULL CHECK(length(trim(product)) > 0),
    account_profile_id TEXT REFERENCES usage_accounts(id) ON DELETE SET NULL,
    identity_status TEXT NOT NULL CHECK(
        identity_status IN ('confirmed', 'unconfirmed', 'unknown')
    ),
    label TEXT NOT NULL CHECK(length(trim(label)) > 0),
    installation_ids_json TEXT NOT NULL CHECK(
        json_valid(installation_ids_json)
        AND json_type(installation_ids_json) = 'array'
    ),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK(identity_status <> 'confirmed' OR account_profile_id IS NOT NULL)
) STRICT;

CREATE UNIQUE INDEX ux_usage_pools_key ON usage_pools(pool_key);
CREATE INDEX ix_usage_pools_agent ON usage_pools(host_id, agent_id, updated_at_ms DESC);

CREATE TABLE usage_snapshots(
    id TEXT PRIMARY KEY,
    pool_id TEXT NOT NULL REFERENCES usage_pools(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK(
        status IN ('ok', 'partial', 'stale', 'unauthenticated', 'unsupported',
                   'rate-limited', 'error', 'unknown')
    ),
    source_kind TEXT NOT NULL CHECK(
        source_kind IN ('official-machine', 'local-transcript', 'manual-entry', 'unavailable')
    ),
    source TEXT NOT NULL CHECK(length(trim(source)) > 0),
    captured_at TEXT NOT NULL CHECK(length(trim(captured_at)) > 0),
    expires_at TEXT CHECK(expires_at IS NULL OR length(trim(expires_at)) > 0),
    error_json TEXT CHECK(error_json IS NULL OR json_valid(error_json)),
    retry_after_ms INTEGER CHECK(retry_after_ms IS NULL OR retry_after_ms >= 0),
    next_attempt_at TEXT CHECK(next_attempt_at IS NULL OR length(trim(next_attempt_at)) > 0),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    note TEXT,
    unavailable_fields_json TEXT NOT NULL DEFAULT '[]' CHECK(
        json_valid(unavailable_fields_json)
        AND json_type(unavailable_fields_json) = 'array'
    ),
    created_at_ms INTEGER NOT NULL
) STRICT;

CREATE INDEX ix_usage_snapshots_pool
    ON usage_snapshots(pool_id, created_at_ms DESC);

CREATE TABLE usage_samples(
    snapshot_id TEXT NOT NULL REFERENCES usage_snapshots(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    metric TEXT NOT NULL CHECK(
        metric IN ('quota-window', 'token-usage', 'credit-balance', 'cost-estimate', 'billing')
    ),
    unit TEXT NOT NULL CHECK(
        unit IN ('percent', 'tokens', 'credits', 'usd-micros', 'count')
    ),
    window TEXT NOT NULL CHECK(
        window IN ('short', 'daily', 'weekly', 'monthly', 'lifetime', 'session', 'unspecified')
    ),
    label TEXT NOT NULL CHECK(length(trim(label)) > 0),
    used_percent REAL CHECK(used_percent IS NULL OR (used_percent >= 0 AND used_percent <= 100)),
    remaining_percent REAL CHECK(
        remaining_percent IS NULL OR (remaining_percent >= 0 AND remaining_percent <= 100)
    ),
    amount REAL,
    total_tokens INTEGER CHECK(total_tokens IS NULL OR total_tokens >= 0),
    input_tokens INTEGER CHECK(input_tokens IS NULL OR input_tokens >= 0),
    cached_input_tokens INTEGER CHECK(cached_input_tokens IS NULL OR cached_input_tokens >= 0),
    cache_creation_tokens INTEGER CHECK(cache_creation_tokens IS NULL OR cache_creation_tokens >= 0),
    output_tokens INTEGER CHECK(output_tokens IS NULL OR output_tokens >= 0),
    token_semantics TEXT CHECK(
        token_semantics IS NULL
        OR token_semantics IN ('input-includes-cached', 'input-excludes-cached')
    ),
    resets_at TEXT,
    window_minutes INTEGER CHECK(window_minutes IS NULL OR window_minutes >= 0),
    estimated INTEGER NOT NULL CHECK(estimated IN (0, 1)),
    source TEXT NOT NULL CHECK(length(trim(source)) > 0),
    note TEXT,
    PRIMARY KEY(snapshot_id, ordinal)
) STRICT;

CREATE TABLE usage_provider_state(
    agent_id TEXT PRIMARY KEY,
    last_attempt_at TEXT,
    last_success_at TEXT,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    backoff_until TEXT,
    last_status TEXT,
    last_error_json TEXT CHECK(last_error_json IS NULL OR json_valid(last_error_json)),
    updated_at_ms INTEGER NOT NULL
) STRICT;

-- User-defined CLIs. `args_json` is an array, never a shell string: the whole
-- point of the S04 argument chain is that a user argument cannot change the
-- command structure. The unique key keeps the same executable+argv pair from
-- being registered twice while still allowing one executable with two presets.
CREATE TABLE custom_clis(
    id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL,
    label TEXT NOT NULL CHECK(length(trim(label)) > 0),
    executable_display_path TEXT NOT NULL CHECK(length(trim(executable_display_path)) > 0),
    executable_canonical_path TEXT NOT NULL CHECK(length(trim(executable_canonical_path)) > 0),
    args_json TEXT NOT NULL CHECK(
        json_valid(args_json) AND json_type(args_json) = 'array'
    ),
    cwd TEXT CHECK(cwd IS NULL OR length(trim(cwd)) > 0),
    product_hint TEXT CHECK(product_hint IS NULL OR length(trim(product_hint)) > 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(host_id, executable_canonical_path, args_json)
) STRICT;

CREATE INDEX ix_custom_clis_order ON custom_clis(host_id, created_at_ms DESC);
"#;

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        description: "core domain schema and user-owned notes/tags",
        statements: &[V1_CORE],
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 2,
        description: "message access indexes for session detail and run tracing",
        statements: &[V2_MESSAGE_ORDER_INDEX, V2_RUN_MESSAGE_INDEX],
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 3,
        description: "installation executable identity and discovery process state",
        statements: &[V3_INSTALLATION_IDENTITY, V3_DISCOVERY_STATE],
        requires_foreign_keys_off: true,
    },
    Migration {
        version: 4,
        description: "read-only history sources, cursors, provenance and FTS5 search index",
        statements: &[V4_HISTORY_INDEX, V4_HISTORY_FTS],
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 5,
        description: "per-file provenance, resumable read state and content verification blocks",
        statements: &[V5_HISTORY_INTEGRITY],
        requires_foreign_keys_off: true,
    },
    Migration {
        version: 6,
        description: "logical and physical content-verification layers per file",
        statements: &[V6_BLOCK_LAYERS],
        requires_foreign_keys_off: true,
    },
    Migration {
        version: 7,
        description: "account profiles, confirmed-account quota pools, usage snapshots and custom CLIs",
        statements: &[V7_USAGE],
        requires_foreign_keys_off: false,
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
    if !migration.requires_foreign_keys_off {
        return apply_migration_inner(conn, migration, fault);
    }
    // `PRAGMA foreign_keys` is a no-op inside a transaction, so a table
    // rebuild toggles it around the transaction and re-verifies every child
    // reference before committing.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let result = apply_migration_inner(conn, migration, fault);
    let restored = conn.pragma_update(None, "foreign_keys", "ON");
    match result {
        Ok(()) => {
            restored?;
            let violations: i64 =
                conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get(0)
                })?;
            if violations > 0 {
                return Err(AppError::new(
                    ErrorCode::Database,
                    format!(
                        "schema migration to version {} left {violations} foreign key violations",
                        migration.version
                    ),
                )
                .with_details(json!({
                    "stage": "migrationForeignKeyCheck",
                    "version": migration.version,
                    "violations": violations,
                })));
            }
            Ok(())
        }
        Err(error) => {
            let _ = restored;
            Err(error)
        }
    }
}

fn apply_migration_inner(
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
    if migration.requires_foreign_keys_off {
        let violations: i64 =
            tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        if violations > 0 {
            return Err(AppError::new(
                ErrorCode::Database,
                format!(
                    "schema migration to version {} would break {violations} foreign key references",
                    migration.version
                ),
            )
            .with_details(json!({
                "stage": "migrationForeignKeyCheck",
                "version": migration.version,
                "violations": violations,
            })));
        }
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
    // Migration safety copies are not user backups: keep them in a subdirectory
    // so the profile backup list never shows an unreadable pseudo-backup.
    let backup_dir = data_root.join("backups").join("migrations");
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
