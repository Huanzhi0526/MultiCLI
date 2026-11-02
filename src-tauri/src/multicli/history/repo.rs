//! SQL for history sources, cursors, provenance and search.
//!
//! All writes that belong to one file import happen inside a caller-provided
//! transaction, so an interrupted import cannot advance a cursor past rows that
//! were never committed.

use super::ids;
use super::model::{HistoryProduct, SearchPosition};
use super::parser::{RecordOut, TitleSource};
use crate::multicli::domain::{
    HostPath, Message, Session, SessionOrigin, SessionWorkspace, SessionWorkspaceRole, Workspace,
};
use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::repo;
use crate::multicli::store::now_ms;
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Row};
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub id: String,
    pub host_id: String,
    pub agent_id: String,
    pub product: String,
    pub installation_id: Option<String>,
    pub config_profile_id: String,
    pub root_display_path: String,
    pub root_canonical_path: String,
    pub enabled: bool,
    pub parser_version: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl SourceRow {
    pub fn root(&self) -> HostPath {
        HostPath {
            host_id: self.host_id.clone(),
            display_path: self.root_display_path.clone(),
            canonical_path: Some(self.root_canonical_path.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStateRow {
    pub source_id: String,
    pub generation: u64,
    pub status: String,
    pub in_flight: bool,
    pub last_attempt_at: Option<String>,
    pub last_success_at: Option<String>,
    pub error_json: Option<String>,
    pub files_total: u64,
    pub files_done: u64,
    pub files_skipped: u64,
    pub files_failed: u64,
    pub messages_indexed: u64,
    pub bytes_read: u64,
    pub complete: bool,
    pub note: Option<String>,
    pub updated_at_ms: i64,
}

impl IndexStateRow {
    pub fn idle(source_id: &str, parser_version: u32) -> Self {
        Self {
            source_id: source_id.to_string(),
            generation: 0,
            status: "idle".to_string(),
            in_flight: false,
            last_attempt_at: None,
            last_success_at: None,
            error_json: None,
            files_total: 0,
            files_done: 0,
            files_skipped: 0,
            files_failed: 0,
            messages_indexed: 0,
            bytes_read: 0,
            complete: false,
            note: Some(format!("parserVersion {parser_version}")),
            updated_at_ms: now_ms(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorRow {
    pub source_id: String,
    pub file_identity: String,
    pub display_path: String,
    pub canonical_path: String,
    pub size_bytes: u64,
    pub mtime_ms: i64,
    pub head_hash: String,
    pub anchor_hash: String,
    pub committed_offset: u64,
    pub tail_residue: String,
    pub tail_residue_truncated: bool,
    pub parser_version: u32,
    pub error_count: u64,
    pub last_error_json: Option<String>,
    /// Parser records consumed so far. Fallback event ids are derived from this
    /// counter, so an append must continue it instead of restarting at 1.
    pub lines_committed: u64,
    /// True only when the reader reached end-of-file during the last pass. A
    /// cursor that stopped on its byte or time budget is not complete even when
    /// `size_bytes` and the mtime are unchanged.
    pub read_complete: bool,
    /// Parser checkpoint (native session id, cwd, extra roots, pending title).
    /// `None` means the cursor predates v5 and its resume state is unknown; the
    /// scanner re-reads that file once rather than guessing.
    pub parser_state_json: Option<String>,
    /// Where the next bounded content-verification window starts. Cycles from 0
    /// to `size_bytes`; a complete cycle has compared every block.
    pub verify_offset: u64,
    /// When the last full cycle finished. A file verified recently is skipped
    /// so the pass budget reaches the files that have not been checked.
    pub verified_at_ms: i64,
}

/// Inserts a source or returns the existing row for the same complete identity
/// (host + agent + profile + canonical root). A configured root is never
/// silently re-pointed: changing the canonical root of an existing id is a
/// conflict, and the caller must create a new source instead.
pub fn upsert_source(
    conn: &Connection,
    host_id: &str,
    product: &HistoryProduct,
    installation_id: Option<&str>,
    config_profile_id: &str,
    root_display_path: &str,
    root_canonical_path: &str,
    enabled: bool,
    requested_id: Option<&str>,
) -> Result<SourceRow, AppError> {
    let id = ids::source_id(host_id, product.agent_id, config_profile_id, root_canonical_path);
    if let Some(existing) = read_source(conn, &id)? {
        if ids::fold_path(&existing.root_canonical_path) != ids::fold_path(root_canonical_path)
            || existing.agent_id != product.agent_id
            || existing.config_profile_id != config_profile_id
            || existing.host_id != host_id
        {
            return Err(AppError::conflict(
                "history source",
                "the source id already belongs to a different root, product or profile",
            )
            .with_details(serde_json::json!({
                "stage": "historySourceIdentity",
                "sourceId": id,
                "existingRootHost": existing.host_id,
                "existingAgentId": existing.agent_id,
                "existingProfile": existing.config_profile_id,
            })));
        }
        conn.execute(
            "UPDATE history_sources
             SET enabled = ?2, root_display_path = ?3, installation_id = ?4, updated_at_ms = ?5
             WHERE id = ?1",
            params![
                id,
                if enabled { 1 } else { 0 },
                root_display_path,
                installation_id,
                now_ms()
            ],
        )?;
        return read_source(conn, &id)?.ok_or_else(|| {
            AppError::new(ErrorCode::Database, "history source disappeared after update")
        });
    }
    if let Some(requested_id) = requested_id {
        if !requested_id.trim().is_empty() && requested_id != id {
            return Err(AppError::conflict(
                "history source",
                "requested source id does not match the configured root identity",
            )
            .with_details(serde_json::json!({
                "stage": "historySourceIdentity",
                "requestedId": requested_id,
                "derivedId": id,
            })));
        }
    }
    conn.execute(
        "INSERT INTO history_sources(
             id, host_id, agent_id, product, installation_id, config_profile_id,
             root_display_path, root_canonical_path, enabled, parser_version,
             created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            id,
            host_id,
            product.agent_id,
            product.product,
            installation_id,
            config_profile_id,
            root_display_path,
            root_canonical_path,
            if enabled { 1 } else { 0 },
            super::model::HISTORY_PARSER_VERSION,
            now_ms(),
            now_ms(),
        ],
    )?;
    read_source(conn, &id)?
        .ok_or_else(|| AppError::new(ErrorCode::Database, "history source disappeared after insert"))
}

fn map_source(row: &Row<'_>) -> rusqlite::Result<SourceRow> {
    Ok(SourceRow {
        id: row.get(0)?,
        host_id: row.get(1)?,
        agent_id: row.get(2)?,
        product: row.get(3)?,
        installation_id: row.get(4)?,
        config_profile_id: row.get(5)?,
        root_display_path: row.get(6)?,
        root_canonical_path: row.get(7)?,
        enabled: row.get::<_, i64>(8)? != 0,
        parser_version: u32::try_from(row.get::<_, i64>(9)?).unwrap_or(1),
        created_at_ms: row.get(10)?,
        updated_at_ms: row.get(11)?,
    })
}

const SOURCE_COLUMNS: &str = "id, host_id, agent_id, product, installation_id, config_profile_id,
     root_display_path, root_canonical_path, enabled, parser_version, created_at_ms, updated_at_ms";

pub fn read_source(conn: &Connection, id: &str) -> Result<Option<SourceRow>, AppError> {
    Ok(conn
        .query_row(
            &format!("SELECT {SOURCE_COLUMNS} FROM history_sources WHERE id = ?1"),
            [id],
            map_source,
        )
        .optional()?)
}

pub fn list_sources(conn: &Connection) -> Result<Vec<SourceRow>, AppError> {
    let mut statement = conn.prepare(&format!(
        "SELECT {SOURCE_COLUMNS} FROM history_sources ORDER BY agent_id, root_display_path, id"
    ))?;
    let rows = statement.query_map([], map_source)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn set_enabled(conn: &Connection, id: &str, enabled: bool) -> Result<SourceRow, AppError> {
    conn.execute(
        "UPDATE history_sources SET enabled = ?2, updated_at_ms = ?3 WHERE id = ?1",
        params![id, if enabled { 1 } else { 0 }, now_ms()],
    )?;
    read_source(conn, id)?.ok_or_else(|| AppError::not_found("history source", id))
}

// ---------------------------------------------------------------------------
// Index state
// ---------------------------------------------------------------------------

fn map_state(row: &Row<'_>) -> rusqlite::Result<IndexStateRow> {
    Ok(IndexStateRow {
        source_id: row.get(0)?,
        generation: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
        status: row.get(2)?,
        in_flight: row.get::<_, i64>(3)? != 0,
        last_attempt_at: row.get(4)?,
        last_success_at: row.get(5)?,
        error_json: row.get(6)?,
        files_total: u64::try_from(row.get::<_, i64>(7)?).unwrap_or(0),
        files_done: u64::try_from(row.get::<_, i64>(8)?).unwrap_or(0),
        files_skipped: u64::try_from(row.get::<_, i64>(9)?).unwrap_or(0),
        files_failed: u64::try_from(row.get::<_, i64>(10)?).unwrap_or(0),
        messages_indexed: u64::try_from(row.get::<_, i64>(11)?).unwrap_or(0),
        bytes_read: u64::try_from(row.get::<_, i64>(12)?).unwrap_or(0),
        complete: row.get::<_, i64>(13)? != 0,
        note: row.get(14)?,
        updated_at_ms: row.get(15)?,
    })
}

pub fn ensure_state(conn: &Connection, source_id: &str) -> Result<IndexStateRow, AppError> {
    if let Some(state) = read_state(conn, source_id)? {
        return Ok(state);
    }
    let state = IndexStateRow::idle(source_id, super::model::HISTORY_PARSER_VERSION);
    write_state(conn, &state)?;
    read_state(conn, source_id)?
        .ok_or_else(|| AppError::new(ErrorCode::Database, "history state disappeared after insert"))
}

pub fn read_state(conn: &Connection, source_id: &str) -> Result<Option<IndexStateRow>, AppError> {
    Ok(conn
        .query_row(
            "SELECT source_id, generation, status, in_flight, last_attempt_at, last_success_at,
                    last_error_json, files_total, files_done, files_skipped, files_failed,
                    messages_indexed, bytes_read, complete, note, updated_at_ms
             FROM history_index_state WHERE source_id = ?1",
            [source_id],
            map_state,
        )
        .optional()?)
}

pub fn write_state(conn: &Connection, state: &IndexStateRow) -> Result<(), AppError> {
    conn.execute(
        "INSERT INTO history_index_state(
             source_id, generation, status, in_flight, last_attempt_at, last_success_at,
             last_error_json, files_total, files_done, files_skipped, files_failed,
             messages_indexed, bytes_read, complete, note, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(source_id) DO UPDATE SET
             generation = excluded.generation,
             status = excluded.status,
             in_flight = excluded.in_flight,
             last_attempt_at = excluded.last_attempt_at,
             last_success_at = excluded.last_success_at,
             last_error_json = excluded.last_error_json,
             files_total = excluded.files_total,
             files_done = excluded.files_done,
             files_skipped = excluded.files_skipped,
             files_failed = excluded.files_failed,
             messages_indexed = excluded.messages_indexed,
             bytes_read = excluded.bytes_read,
             complete = excluded.complete,
             note = excluded.note,
             updated_at_ms = excluded.updated_at_ms",
        params![
            state.source_id.as_str(),
            state.generation as i64,
            state.status.as_str(),
            if state.in_flight { 1 } else { 0 },
            state.last_attempt_at.as_deref(),
            state.last_success_at.as_deref(),
            state.error_json.as_deref(),
            state.files_total as i64,
            state.files_done as i64,
            state.files_skipped as i64,
            state.files_failed as i64,
            state.messages_indexed as i64,
            state.bytes_read as i64,
            if state.complete { 1 } else { 0 },
            state.note.as_deref(),
            now_ms(),
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Cursors
// ---------------------------------------------------------------------------

fn map_cursor(row: &Row<'_>) -> rusqlite::Result<CursorRow> {
    Ok(CursorRow {
        source_id: row.get(0)?,
        file_identity: row.get(1)?,
        display_path: row.get(2)?,
        canonical_path: row.get(3)?,
        size_bytes: u64::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
        mtime_ms: row.get(5)?,
        head_hash: row.get(6)?,
        anchor_hash: row.get(7)?,
        committed_offset: u64::try_from(row.get::<_, i64>(8)?).unwrap_or(0),
        tail_residue: row.get(9)?,
        tail_residue_truncated: row.get::<_, i64>(10)? != 0,
        parser_version: u32::try_from(row.get::<_, i64>(11)?).unwrap_or(1),
        error_count: u64::try_from(row.get::<_, i64>(12)?).unwrap_or(0),
        last_error_json: row.get(13)?,
        lines_committed: u64::try_from(row.get::<_, i64>(14)?).unwrap_or(0),
        read_complete: row.get::<_, i64>(15)? != 0,
        parser_state_json: row.get(16)?,
        verify_offset: u64::try_from(row.get::<_, i64>(17)?).unwrap_or(0),
        verified_at_ms: row.get(18)?,
    })
}

const CURSOR_COLUMNS: &str = "source_id, file_identity, display_path, canonical_path, size_bytes,
     mtime_ms, head_hash, anchor_hash, committed_offset, tail_residue, tail_residue_truncated,
     parser_version, error_count, last_error_json, lines_committed, read_complete,
     parser_state_json, verify_offset, verified_at_ms";

/// The cursor of one listed path, read fresh.
///
/// The verification phase runs after the import phase, so the map loaded at the
/// start of the pass can already be stale for a file that was just imported.
pub fn read_cursor_by_path(
    conn: &Connection,
    source_id: &str,
    canonical_path: &str,
) -> Result<Option<CursorRow>, AppError> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {CURSOR_COLUMNS} FROM history_cursors
                 WHERE source_id = ?1 AND canonical_path = ?2"
            ),
            params![source_id, canonical_path],
            map_cursor,
        )
        .optional()?)
}

#[cfg(test)]
pub fn read_cursor(
    conn: &Connection,
    source_id: &str,
    file_identity: &str,
) -> Result<Option<CursorRow>, AppError> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {CURSOR_COLUMNS} FROM history_cursors
                 WHERE source_id = ?1 AND file_identity = ?2"
            ),
            params![source_id, file_identity],
            map_cursor,
        )
        .optional()?)
}

/// Every cursor of one source, keyed by canonical path.
///
/// The scanner checks one cursor per listed file. Reading them one query at a
/// time turned a 10k-file pass into 10k round trips and starved the parse
/// budget; one statement keeps the pass linear in file size instead.
pub fn cursors_for_source(
    conn: &Connection,
    source_id: &str,
) -> Result<std::collections::HashMap<String, CursorRow>, AppError> {
    let mut statement = conn.prepare(&format!(
        "SELECT {CURSOR_COLUMNS} FROM history_cursors WHERE source_id = ?1"
    ))?;
    let rows = statement.query_map([source_id], map_cursor)?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let cursor = row?;
        out.insert(cursor.canonical_path.clone(), cursor);
    }
    Ok(out)
}

/// Clears the in-flight flag a previous process left behind.
///
/// The flag lives in the database so a restarted app can report progress, but
/// nothing survives a crash to clear it. This engine is the only writer for its
/// host, so at start-up any set flag is stale by definition. The status becomes
/// `cancelled` (not `ok`) because the pass really did stop mid-way.
pub fn reset_in_flight(conn: &mut Connection, host_id: &str) -> Result<u64, AppError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE history_index_state
         SET in_flight = 0,
             status = CASE WHEN status = 'indexing' THEN 'cancelled' ELSE status END,
             note = CASE WHEN status = 'indexing'
                     THEN 'the previous process ended mid-pass; the next pass resumes from the committed cursors'
                     ELSE note END,
             updated_at_ms = ?2
         WHERE in_flight = 1
           AND source_id IN (SELECT id FROM history_sources WHERE host_id = ?1)",
        params![host_id, now_ms()],
    )?;
    tx.commit()?;
    Ok(u64::try_from(changed).unwrap_or(0))
}

pub fn write_cursor(conn: &Connection, cursor: &CursorRow) -> Result<(), AppError> {
    conn.execute(
        "INSERT INTO history_cursors(
             source_id, file_identity, display_path, canonical_path, size_bytes, mtime_ms,
             head_hash, anchor_hash, committed_offset, tail_residue, tail_residue_truncated,
             parser_version, error_count, last_error_json, lines_committed, read_complete,
             parser_state_json, verify_offset, verified_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                   ?17, ?18, ?19, ?20)
         ON CONFLICT(source_id, file_identity) DO UPDATE SET
             display_path = excluded.display_path,
             canonical_path = excluded.canonical_path,
             size_bytes = excluded.size_bytes,
             mtime_ms = excluded.mtime_ms,
             head_hash = excluded.head_hash,
             anchor_hash = excluded.anchor_hash,
             committed_offset = excluded.committed_offset,
             tail_residue = excluded.tail_residue,
             tail_residue_truncated = excluded.tail_residue_truncated,
             parser_version = excluded.parser_version,
             error_count = excluded.error_count,
             last_error_json = excluded.last_error_json,
             lines_committed = excluded.lines_committed,
             read_complete = excluded.read_complete,
             parser_state_json = excluded.parser_state_json,
             verify_offset = excluded.verify_offset,
             verified_at_ms = excluded.verified_at_ms,
             updated_at_ms = excluded.updated_at_ms",
        params![
            cursor.source_id.as_str(),
            cursor.file_identity.as_str(),
            cursor.display_path.as_str(),
            cursor.canonical_path.as_str(),
            cursor.size_bytes as i64,
            cursor.mtime_ms,
            cursor.head_hash.as_str(),
            cursor.anchor_hash.as_str(),
            cursor.committed_offset as i64,
            cursor.tail_residue.as_str(),
            if cursor.tail_residue_truncated { 1 } else { 0 },
            cursor.parser_version as i64,
            cursor.error_count as i64,
            cursor.last_error_json.as_deref(),
            cursor.lines_committed as i64,
            if cursor.read_complete { 1 } else { 0 },
            cursor.parser_state_json.as_deref(),
            cursor.verify_offset as i64,
            cursor.verified_at_ms,
            now_ms(),
        ],
    )?;
    Ok(())
}

/// Records how far the bounded verification cycle has walked, and refreshes
/// the observed mtime once the bytes matched.
///
/// `size_bytes` is deliberately *not* updated here. It is the file length the
/// last import actually saw; a verification pass may be looking at a file the
/// import phase did not reach this pass, and writing the newer listing length
/// would make `decide` treat the still-unimported tail as already handled — a
/// permanent skip, because the prefix hash ignores the length.
pub fn set_verify_progress(
    conn: &Connection,
    source_id: &str,
    file_identity: &str,
    verify_offset: u64,
    mtime_ms: i64,
) -> Result<(), AppError> {
    let now = now_ms();
    conn.execute(
        "UPDATE history_cursors
         SET verify_offset = ?3, mtime_ms = ?4,
             verified_at_ms = CASE WHEN ?3 = 0 THEN ?5 ELSE verified_at_ms END,
             updated_at_ms = ?5
         WHERE source_id = ?1 AND file_identity = ?2",
        params![
            source_id,
            file_identity,
            verify_offset as i64,
            mtime_ms,
            now,
        ],
    )?;
    Ok(())
}

pub fn delete_cursor(
    conn: &Connection,
    source_id: &str,
    file_identity: &str,
) -> Result<(), AppError> {
    conn.execute(
        "DELETE FROM history_cursors WHERE source_id = ?1 AND file_identity = ?2",
        params![source_id, file_identity],
    )?;
    Ok(())
}

/// Removes every cursor whose canonical path is no longer part of the tree and
/// purges what those files contributed.
///
/// Without this a deleted-and-recreated file would inherit a stale offset, and
/// the rows of a deleted transcript would stay in the index forever (a truncate
/// to zero was handled, an unlink was not).
///
/// Returns `(cursors_removed, messages_purged)`.
pub fn purge_missing_cursors(
    conn: &mut Connection,
    source_id: &str,
    canonical_paths: &[String],
) -> Result<(u64, u64), AppError> {
    let existing: Vec<(String, String)> = {
        let statement = conn.prepare(
            "SELECT file_identity, canonical_path FROM history_cursors WHERE source_id = ?1",
        )?;
        let mut statement = statement;
        let rows = statement.query_map([source_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    // A hash set, not a linear scan: with 10k cursors and 10k listed paths the
    // quadratic compare alone cost about a second of every pass.
    let listed: std::collections::HashSet<&str> =
        canonical_paths.iter().map(String::as_str).collect();
    let missing: Vec<String> = existing
        .into_iter()
        .filter(|(_, canonical)| !listed.contains(canonical.as_str()))
        .map(|(identity, _)| identity)
        .collect();
    if missing.is_empty() {
        return Ok((0, 0));
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut purged = 0_u64;
    for identity in &missing {
        purged += purge_origins_and_orphans(&tx, source_id, Some(identity))?;
        delete_file_blocks(&tx, source_id, identity)?;
        tx.execute(
            "DELETE FROM history_cursors WHERE source_id = ?1 AND file_identity = ?2",
            params![source_id, identity],
        )?;
    }
    if purged > 0 {
        bump_purge_epoch(&tx)?;
    }
    tx.commit()?;
    Ok((missing.len() as u64, purged))
}

// ---------------------------------------------------------------------------
// Purge (derived only)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PurgeReport {
    pub messages_deleted: u64,
    pub origins_deleted: u64,
    pub cursors_deleted: u64,
    pub sessions_kept: u64,
}

/// Message ids that this file (or the whole source when `file_identity` is
/// `None`) contributed at least one origin row for.
fn contributing_message_ids(
    conn: &Connection,
    source_id: &str,
    file_identity: Option<&str>,
) -> Result<Vec<String>, AppError> {
    let mut sql = String::from(
        "SELECT DISTINCT message_id FROM history_message_origins WHERE source_id = ?1",
    );
    if file_identity.is_some() {
        sql.push_str(" AND file_identity = ?2");
    }
    let mut statement = conn.prepare(&sql)?;
    let rows = match file_identity {
        Some(identity) => statement
            .query_map(params![source_id, identity], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?,
        None => statement
            .query_map([source_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?,
    };
    Ok(rows)
}

/// Deletes a message only when no origin row anywhere still references it.
///
/// This is the whole point of the v5 primary key: two transcripts of the same
/// source can carry the same event, and dropping one of them must not remove
/// the message the other still provides. Deletes are batched because the FTS
/// `message_id` column is UNINDEXED, so a per-row delete scans the index.
fn delete_messages_without_origins(
    conn: &Connection,
    ids: &[String],
) -> Result<u64, AppError> {
    let mut deleted = 0_u64;
    for chunk in ids.chunks(400) {
        let placeholders = std::iter::repeat("?")
            .take(chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        conn.execute(
            &format!(
                "DELETE FROM history_message_fts WHERE message_id IN ({placeholders})
                 AND NOT EXISTS (
                     SELECT 1 FROM history_message_origins o
                     WHERE o.message_id = history_message_fts.message_id
                 )"
            ),
            params_from_iter(chunk.iter().map(String::as_str)),
        )?;
        deleted += conn.execute(
            &format!(
                "DELETE FROM messages WHERE id IN ({placeholders})
                 AND NOT EXISTS (
                     SELECT 1 FROM history_message_origins o
                     WHERE o.message_id = messages.id
                 )"
            ),
            params_from_iter(chunk.iter().map(String::as_str)),
        )? as u64;
    }
    Ok(deleted)
}

/// Removes the origin rows of one file (or one source) and then every message
/// left without a contributor. Returns the number of messages removed.
fn purge_origins_and_orphans(
    conn: &Connection,
    source_id: &str,
    file_identity: Option<&str>,
) -> Result<u64, AppError> {
    let ids = contributing_message_ids(conn, source_id, file_identity)?;
    match file_identity {
        Some(identity) => {
            conn.execute(
                "DELETE FROM history_message_origins
                 WHERE source_id = ?1 AND file_identity = ?2",
                params![source_id, identity],
            )?;
        }
        None => {
            conn.execute(
                "DELETE FROM history_message_origins WHERE source_id = ?1",
                [source_id],
            )?;
        }
    }
    refresh_source_refs(conn, &ids)?;
    delete_messages_without_origins(conn, &ids)
}

/// Re-points a surviving message at a file that still contributes it.
///
/// `messages.source_ref` is a single column (frozen schema) and used to keep
/// the first file that imported the event. When that file is truncated or
/// removed while another one still provides the message, the reference would
/// name a file that no longer contributes — and the next pass skips the
/// message, so it would never be repaired.
fn refresh_source_refs(conn: &Connection, ids: &[String]) -> Result<(), AppError> {
    for chunk in ids.chunks(400) {
        let placeholders = std::iter::repeat("?")
            .take(chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        conn.execute(
            &format!(
                "UPDATE messages SET source_ref = (
                     SELECT substr(o.file_identity, 1, 24) || '#L' || o.line_index
                     FROM history_message_origins o
                     WHERE o.message_id = messages.id
                     ORDER BY o.file_identity, o.line_index LIMIT 1
                 )
                 WHERE id IN ({placeholders})
                   AND EXISTS (
                       SELECT 1 FROM history_message_origins o
                       WHERE o.message_id = messages.id
                   )"
            ),
            params_from_iter(chunk.iter().map(String::as_str)),
        )?;
    }
    Ok(())
}

/// Removes only what this source derived. Sessions, their ids, user notes and
/// tags stay: a rebuild must not make the user's own annotations disappear.
pub fn purge_source_derived(
    conn: &mut Connection,
    source_id: &str,
) -> Result<PurgeReport, AppError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let sessions_kept: i64 = tx.query_row(
        "SELECT COUNT(DISTINCT f.session_id) FROM history_session_facts f WHERE f.source_id = ?1",
        [source_id],
        |row| row.get(0),
    )?;
    let origins_deleted: i64 = tx.query_row(
        "SELECT COUNT(*) FROM history_message_origins WHERE source_id = ?1",
        [source_id],
        |row| row.get(0),
    )?;
    let messages_deleted = purge_origins_and_orphans(&tx, source_id, None)?;
    let cursors_deleted = tx.execute(
        "DELETE FROM history_cursors WHERE source_id = ?1",
        [source_id],
    )?;
    tx.execute(
        "DELETE FROM history_file_blocks WHERE source_id = ?1",
        [source_id],
    )?;
    if messages_deleted > 0 {
        bump_purge_epoch(&tx)?;
    }
    tx.commit()?;
    Ok(PurgeReport {
        messages_deleted,
        origins_deleted: origins_deleted as u64,
        cursors_deleted: cursors_deleted as u64,
        sessions_kept: u64::try_from(sessions_kept).unwrap_or(0),
    })
}

/// Removes what one file derived, used when a file was truncated or replaced
/// (the previous rows no longer describe the bytes on disk).
///
/// Other files of the same source that provided the same event keep it: only
/// messages left with zero contributors disappear.
pub fn purge_file_derived(
    conn: &Connection,
    source_id: &str,
    file_identity: &str,
) -> Result<u64, AppError> {
    let deleted = purge_origins_and_orphans(conn, source_id, Some(file_identity))?;
    delete_file_blocks(conn, source_id, file_identity)?;
    if deleted > 0 {
        bump_purge_epoch(conn)?;
    }
    Ok(deleted)
}

// ---------------------------------------------------------------------------
// Content verification blocks and the purge epoch
// ---------------------------------------------------------------------------

/// Content hash of one fixed-size window of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRow {
    pub index: u64,
    pub hash: String,
    pub bytes: u64,
}

/// Which byte sequence a block table describes.
///
/// `Logical` covers the decoded records a plain transcript already holds; that
/// is what a normal `.jsonl` is verified against. `Physical` covers the bytes
/// exactly as they sit on disk, which is the only layer that lets a compressed
/// transcript be verified in bounded windows: decoding it from the start for
/// every window would make the cost quadratic, and refusing to decode made a
/// large file permanently unverifiable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockLayer {
    Logical,
    Physical,
}

impl BlockLayer {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockLayer::Logical => "logical",
            BlockLayer::Physical => "physical",
        }
    }
}

/// Stores the blocks of one file from `from_index` on. Blocks before it stay,
/// because an append only re-hashes the block it starts in.
pub fn write_file_blocks(
    conn: &Connection,
    source_id: &str,
    file_identity: &str,
    layer: BlockLayer,
    from_index: u64,
    blocks: &[BlockRow],
) -> Result<(), AppError> {
    conn.execute(
        "DELETE FROM history_file_blocks
         WHERE source_id = ?1 AND file_identity = ?2 AND layer = ?3 AND block_index >= ?4",
        params![source_id, file_identity, layer.as_str(), from_index as i64],
    )?;
    let mut statement = conn.prepare(
        "INSERT INTO history_file_blocks(source_id, file_identity, layer, block_index, hash, bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(source_id, file_identity, layer, block_index) DO UPDATE SET
             hash = excluded.hash,
             bytes = excluded.bytes",
    )?;
    for block in blocks {
        statement.execute(params![
            source_id,
            file_identity,
            layer.as_str(),
            block.index as i64,
            block.hash.as_str(),
            block.bytes as i64,
        ])?;
    }
    Ok(())
}

pub fn read_file_blocks(
    conn: &Connection,
    source_id: &str,
    file_identity: &str,
    layer: BlockLayer,
) -> Result<Vec<BlockRow>, AppError> {
    let mut statement = conn.prepare(
        "SELECT block_index, hash, bytes FROM history_file_blocks
         WHERE source_id = ?1 AND file_identity = ?2 AND layer = ?3 ORDER BY block_index",
    )?;
    let rows = statement.query_map(params![source_id, file_identity, layer.as_str()], |row| {
        Ok(BlockRow {
            index: u64::try_from(row.get::<_, i64>(0)?).unwrap_or(0),
            hash: row.get(1)?,
            bytes: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn delete_file_blocks(
    conn: &Connection,
    source_id: &str,
    file_identity: &str,
) -> Result<(), AppError> {
    conn.execute(
        "DELETE FROM history_file_blocks WHERE source_id = ?1 AND file_identity = ?2",
        params![source_id, file_identity],
    )?;
    Ok(())
}

/// Removes one layer only, leaving the other baseline intact.
pub fn delete_block_layer(
    conn: &Connection,
    source_id: &str,
    file_identity: &str,
    layer: BlockLayer,
) -> Result<(), AppError> {
    conn.execute(
        "DELETE FROM history_file_blocks
         WHERE source_id = ?1 AND file_identity = ?2 AND layer = ?3",
        params![source_id, file_identity, layer.as_str()],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Verification rotation
// ---------------------------------------------------------------------------

/// Where the next pass starts looking for a file to verify.
///
/// Verification costs reads and the pass budget is shared. Always starting at
/// the first path spent the budget on the same files: with a small budget the
/// tail of a corpus never got a turn, and one file whose cycle is in progress
/// could hold the budget forever. The position is persisted so a restart does
/// not reset the rotation.
pub fn verify_path(conn: &Connection, source_id: &str) -> Result<Option<String>, AppError> {
    let key = format!("verifyPath:{source_id}");
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM history_meta WHERE key = ?1",
            [key.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.filter(|path| !path.is_empty()))
}

pub fn set_verify_path(conn: &Connection, source_id: &str, path: &str) -> Result<(), AppError> {
    conn.execute(
        "INSERT INTO history_meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![format!("verifyPath:{source_id}"), path],
    )?;
    Ok(())
}

/// Counter bumped whenever derived rows are actually removed.
///
/// A search or message cursor is a position in a result set; if rows behind
/// that position disappeared (a rebuild, a truncated file), continuing would
/// silently skip records. The epoch makes that a explicit `AppError` instead.
pub fn purge_epoch(conn: &Connection) -> Result<u64, AppError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM history_meta WHERE key = 'purgeEpoch'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.and_then(|text| text.parse().ok()).unwrap_or(0))
}

fn bump_purge_epoch(conn: &Connection) -> Result<(), AppError> {
    conn.execute(
        "INSERT INTO history_meta(key, value) VALUES ('purgeEpoch', '1')
         ON CONFLICT(key) DO UPDATE
             SET value = CAST(CAST(history_meta.value AS INTEGER) + 1 AS TEXT)",
        [],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Ingest
// ---------------------------------------------------------------------------

pub struct IngestContext<'a> {
    pub source: &'a SourceRow,
    pub namespace: &'a str,
    pub file_identity: &'a str,
    pub byte_offset: u64,
    pub line_index: u64,
    pub observed_at: &'a str,
    pub primary_workspace: Option<&'a Workspace>,
    pub related_workspaces: &'a [Workspace],
}

#[derive(Debug, Clone)]
pub struct IngestOutcome {
    pub session_id: String,
    pub inserted: bool,
}

/// One record is normalised inside a savepoint: a rejected row (for example a
/// workspace that disappeared between stat and insert) leaves no message, no
/// provenance, no FTS row and no session behind.
pub fn ingest_record(
    conn: &Connection,
    ctx: &IngestContext<'_>,
    record: &RecordOut,
) -> Result<IngestOutcome, AppError> {
    conn.execute_batch("SAVEPOINT history_record")?;
    let result = ingest_record_inner(conn, ctx, record);
    match &result {
        Ok(_) => {
            conn.execute_batch("RELEASE history_record")?;
        }
        Err(_) => {
            conn.execute_batch("ROLLBACK TO history_record; RELEASE history_record")?;
        }
    }
    result
}

fn ingest_record_inner(
    conn: &Connection,
    ctx: &IngestContext<'_>,
    record: &RecordOut,
) -> Result<IngestOutcome, AppError> {
    let session_id = ids::session_id(ctx.namespace, &record.native_session_id);
    let title = record
        .title_update
        .as_ref()
        .map(|(title, _)| title.clone())
        .unwrap_or_default();

    // A native title always wins; a first-message guess never overwrites it.
    let existing_title_source: Option<String> = conn
        .query_row(
            "SELECT title_source FROM history_session_facts
             WHERE session_id = ?1 AND source_id = ?2",
            params![session_id.as_str(), ctx.source.id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let keep_title = match (&existing_title_source, record.title_update.as_ref()) {
        (Some(existing), Some((_, TitleSource::FirstUserMessage))) => existing == "native-title",
        _ => false,
    };
    let use_title = if keep_title { String::new() } else { title };

    let session = Session {
        id: session_id.clone(),
        agent_id: ctx.source.agent_id.clone(),
        host_id: ctx.source.host_id.clone(),
        source_namespace: ctx.namespace.to_string(),
        native_id: Some(record.native_session_id.clone()),
        origin: SessionOrigin::HistoryOnly,
        title: use_title.clone(),
    };
    // The session starts when its first record was written, not when the source
    // was configured: using the source timestamp gave every session the same
    // `createdAt`, which also flattened the search ordering.
    let session_created_at =
        ids::parse_iso_millis(ctx.observed_at).unwrap_or(ctx.source.created_at_ms);
    repo::ensure_session_in(conn, &session, session_created_at)?;
    if !use_title.trim().is_empty() {
        repo::set_session_title(conn, &session_id, &use_title)?;
    }

    let message_id = ids::message_id(&session_id, &record.source_key);
    let message = Message {
        id: message_id.clone(),
        session_id: session_id.clone(),
        run_id: None,
        native_event_id: record.native_event_id.clone(),
        source_key: record.source_key.clone(),
        seq: record.seq,
        kind: record.kind.clone(),
        content: record.content.clone(),
        source_ref: Some(format!(
            "{}#L{}",
            &ctx.file_identity[..ctx.file_identity.len().min(24)],
            ctx.line_index
        )),
        observed_at: ctx.observed_at.to_string(),
    };
    message.validate()?;
    let (stored, inserted) = repo::insert_message_with_flag_in(conn, &message)?;

    conn.execute(
        "INSERT INTO history_message_origins(
             message_id, source_id, file_identity, byte_offset, line_index, parser_version,
             created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(message_id, source_id, file_identity) DO UPDATE SET
             byte_offset = excluded.byte_offset,
             line_index = excluded.line_index,
             parser_version = excluded.parser_version",
        params![
            message_id,
            ctx.source.id.as_str(),
            ctx.file_identity,
            ctx.byte_offset as i64,
            ctx.line_index as i64,
            super::model::HISTORY_PARSER_VERSION,
            now_ms(),
        ],
    )?;

    // `messages` uses ON CONFLICT DO NOTHING, so an existing row keeps its
    // original content. The FTS row must mirror that stored body — rewriting it
    // from the *new* record would let a search match a word that is not in the
    // message the user opens. A duplicate event therefore adds provenance only.
    if inserted {
        // Ordering keys are denormalised into the FTS row. A broad query can
        // match every message; joining 100k matches back to `messages` twice
        // (count and page) cost ~1.3 s, while ordering on the FTS row and
        // hydrating only the returned page keeps the query under the target.
        let session_created_at: i64 = conn.query_row(
            "SELECT created_at_ms FROM sessions WHERE id = ?1",
            [session_id.as_str()],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT INTO history_message_fts(
                 message_id, session_id, body, observed_at, seq, session_created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                message_id,
                session_id.as_str(),
                record.search_text.as_str(),
                stored.observed_at.as_str(),
                stored.seq as i64,
                session_created_at,
            ],
        )?;
    }

    let title_source = record
        .title_update
        .as_ref()
        .map(|(_, source)| source.as_str())
        .unwrap_or(existing_title_source.as_deref().unwrap_or("none"));
    let title_source = if keep_title {
        existing_title_source.as_deref().unwrap_or("none")
    } else {
        title_source
    };
    conn.execute(
        "INSERT INTO history_session_facts(
             session_id, source_id, native_id, title_source, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(session_id, source_id) DO UPDATE SET
             native_id = excluded.native_id,
             title_source = excluded.title_source,
             updated_at_ms = excluded.updated_at_ms",
        params![
            session_id,
            ctx.source.id.as_str(),
            record.native_session_id.as_str(),
            title_source,
            now_ms(),
            now_ms(),
        ],
    )?;

    if let Some(workspace) = ctx.primary_workspace {
        attach(conn, &session_id, workspace, SessionWorkspaceRole::Primary, "history-cwd")?;
    }
    for workspace in ctx.related_workspaces {
        if ctx
            .primary_workspace
            .map(|primary| primary.path.canonical_path == workspace.path.canonical_path)
            .unwrap_or(false)
        {
            continue;
        }
        attach(
            conn,
            &session_id,
            workspace,
            SessionWorkspaceRole::Related,
            "history-workspace-root",
        )?;
    }

    Ok(IngestOutcome {
        session_id,
        inserted,
    })
}

fn attach(
    conn: &Connection,
    session_id: &str,
    workspace: &Workspace,
    role: SessionWorkspaceRole,
    evidence: &str,
) -> Result<(), AppError> {
    // A history workspace is registered on first sight, keyed by its canonical
    // path, so the id stays stable across passes and other sessions share it.
    let stored = repo::ensure_workspace_by_identity_in(conn, workspace)?;
    let link = SessionWorkspace {
        session_id: session_id.to_string(),
        workspace_id: stored.id,
        role,
        evidence: evidence.to_string(),
    };
    repo::attach_workspace_in(conn, &link)?;
    Ok(())
}

/// Applies a native title to a session that already exists. Returns false when
/// the session has not been created yet, in which case the caller keeps the
/// title pending and applies it with the first record.
pub fn apply_native_title(
    conn: &Connection,
    session_id: &str,
    source_id: &str,
    title: &str,
) -> Result<bool, AppError> {
    if title.trim().is_empty() {
        return Ok(true);
    }
    let exists: Option<String> = conn
        .query_row("SELECT id FROM sessions WHERE id = ?1", [session_id], |row| {
            row.get(0)
        })
        .optional()?;
    if exists.is_none() {
        return Ok(false);
    }
    repo::set_session_title(conn, session_id, title)?;
    conn.execute(
        "UPDATE history_session_facts SET title_source = 'native-title', updated_at_ms = ?3
         WHERE session_id = ?1 AND source_id = ?2",
        params![session_id, source_id, now_ms()],
    )?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SearchRow {
    pub message: Message,
    pub session_title: String,
    pub agent_id: String,
    pub session_created_at: i64,
    pub session_message_count: u64,
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    Fts,
    Like,
}

impl MatchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fts => "fts",
            Self::Like => "like",
        }
    }
}

pub struct SearchFilter<'a> {
    pub workspace_id: Option<&'a str>,
    pub agent_id: Option<&'a str>,
    pub source_id: Option<&'a str>,
}

/// Escapes LIKE metacharacters. Backslash is the escape character.
pub fn escape_like(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '%' => out.push_str("\\%"),
            '_' => out.push_str("\\_"),
            other => out.push(other),
        }
    }
    out
}

/// Builds an FTS5 phrase query. Wrapping the user text in double quotes makes
/// every FTS operator literal (`*`, `-`, `:`, `^`, `(`, `NEAR`), and doubling an
/// inner quote is the FTS5 escape for a quote inside a phrase.
pub fn fts_phrase(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

/// A trigram index cannot answer a query shorter than three characters, so
/// those go through the LIKE fallback instead of returning a misleading zero.
pub fn match_mode_for(text: &str) -> MatchMode {
    if text.chars().count() >= 3 {
        MatchMode::Fts
    } else {
        MatchMode::Like
    }
}

pub fn search_messages(
    conn: &Connection,
    text: &str,
    filter: &SearchFilter<'_>,
    mode: MatchMode,
    cursor: Option<&SearchPosition>,
    fetch_limit: usize,
) -> Result<(Vec<SearchRow>, u64), AppError> {
    let (rows, total, missing) =
        search_messages_on_fts(conn, text, filter, mode, cursor, fetch_limit)?;
    if missing == 0 {
        return Ok((rows, total));
    }
    // The FTS row exists but its message does not (only possible if a session
    // was deleted outside the history module). The join path is authoritative:
    // take it rather than return a page with holes or an inflated total.
    search_messages_joined(conn, text, filter, mode, cursor, fetch_limit)
}

fn search_messages_joined(
    conn: &Connection,
    text: &str,
    filter: &SearchFilter<'_>,
    mode: MatchMode,
    cursor: Option<&SearchPosition>,
    fetch_limit: usize,
) -> Result<(Vec<SearchRow>, u64), AppError> {
    let (where_sql, mut values) = search_where(text, filter, mode);
    let total: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM history_message_fts
             JOIN messages m ON m.id = history_message_fts.message_id
             JOIN sessions s ON s.id = m.session_id
             {where_sql}"
        ),
        params_from_iter(values.iter()),
        |row| row.get(0),
    )?;

    let mut sql = format!(
        "SELECT m.id, m.session_id, m.run_id, m.native_event_id, m.source_key, m.seq, m.kind,
                m.content_json, m.source_ref, m.observed_at,
                s.title, s.agent_id, s.created_at_ms,
                (SELECT COUNT(*) FROM messages mm WHERE mm.session_id = m.session_id)
         FROM history_message_fts
         JOIN messages m ON m.id = history_message_fts.message_id
         JOIN sessions s ON s.id = m.session_id
         {where_sql}"
    );
    if let Some(position) = cursor {
        sql.push_str(
            " AND (s.created_at_ms < ? OR (s.created_at_ms = ? AND (
                 m.observed_at > ? OR (m.observed_at = ? AND (
                     m.seq > ? OR (m.seq = ? AND m.id > ?)
                 ))
             )))",
        );
        for _ in 0..2 {
            values.push(Value::Integer(position.session_created_at));
        }
        for _ in 0..2 {
            values.push(Value::Text(position.observed_at.clone()));
        }
        for _ in 0..2 {
            values.push(Value::Integer(position.seq as i64));
        }
        values.push(Value::Text(position.id.clone()));
    }
    sql.push_str(" ORDER BY s.created_at_ms DESC, m.observed_at ASC, m.seq ASC, m.id ASC LIMIT ?");
    values.push(Value::Integer(fetch_limit as i64));

    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), |row| {
        let message = map_search_message(row)?;
        Ok(SearchRow {
            message,
            session_title: row.get(10)?,
            agent_id: row.get(11)?,
            session_created_at: row.get(12)?,
            session_message_count: u64::try_from(row.get::<_, i64>(13)?).unwrap_or(0),
            source_ids: Vec::new(),
        })
    })?;
    let mut rows = rows.collect::<Result<Vec<_>, _>>()?;
    assign_source_ids(conn, &mut rows)?;
    Ok((rows, u64::try_from(total).unwrap_or(0)))
}

/// Count, order and page on the FTS row, then hydrate only the returned page.
///
/// A query can match every message. Joining 100k matches back to `messages`
/// twice (count and page) measured about 1.3 s; the FTS row already carries the
/// ordering keys and the session id. Materialize the filtered matches once,
/// count before applying the cursor, and hydrate only the bounded page. The
/// third return value is the
/// number of FTS rows whose message was not found, so the caller can fall back
/// to the authoritative join path.
fn search_messages_on_fts(
    conn: &Connection,
    text: &str,
    filter: &SearchFilter<'_>,
    mode: MatchMode,
    cursor: Option<&SearchPosition>,
    fetch_limit: usize,
) -> Result<(Vec<SearchRow>, u64, usize), AppError> {
    let (match_sql, match_value) = match_mode_clause(text, mode);
    let mut where_sql = format!(" WHERE {match_sql}");
    let mut prefilter: Vec<Value> = Vec::new();
    if let Some(agent_id) = filter.agent_id {
        where_sql.push_str(" AND session_id IN (SELECT id FROM sessions WHERE agent_id = ?)");
        prefilter.push(Value::Text(agent_id.to_string()));
    }
    if let Some(workspace_id) = filter.workspace_id {
        where_sql.push_str(
            " AND session_id IN (
                 SELECT session_id FROM session_workspaces WHERE workspace_id = ?
             )",
        );
        prefilter.push(Value::Text(workspace_id.to_string()));
    }
    if let Some(source_id) = filter.source_id {
        // `IN (subquery)`, not a correlated `EXISTS`: SQLite builds a bloom
        // filter for the subquery and probes it instead of one b-tree seek per
        // matched row (measured 112 ms vs 231 ms at 40k matches).
        where_sql.push_str(
            " AND message_id IN (
                 SELECT message_id FROM history_message_origins WHERE source_id = ?
             )",
        );
        prefilter.push(Value::Text(source_id.to_string()));
    }

    // Materialize the filtered matches once for both total and page. Repeating
    // the FTS scan and the source membership set made broad filtered searches
    // exceed one second at 100k messages. The total precedes the cursor filter;
    // LEFT JOIN also returns it when the requested page has no more rows.
    let mut sql = format!(
        "WITH matched AS MATERIALIZED (
             SELECT message_id, session_created_at, observed_at, seq
             FROM history_message_fts{where_sql}
         ) SELECT page.message_id, totals.total
         FROM (SELECT COUNT(*) AS total FROM matched) totals
         LEFT JOIN (SELECT * FROM matched WHERE 1 = 1"
    );
    let mut values = vec![Value::Text(match_value)];
    values.extend(prefilter);
    if let Some(position) = cursor {
        sql.push_str(
            " AND (CAST(session_created_at AS INTEGER) < ? OR (
                 CAST(session_created_at AS INTEGER) = ? AND (
                 observed_at > ? OR (observed_at = ? AND (
                     CAST(seq AS INTEGER) > ? OR (CAST(seq AS INTEGER) = ? AND message_id > ?)
                 ))
             )))",
        );
        for _ in 0..2 {
            values.push(Value::Integer(position.session_created_at));
        }
        for _ in 0..2 {
            values.push(Value::Text(position.observed_at.clone()));
        }
        for _ in 0..2 {
            values.push(Value::Integer(position.seq as i64));
        }
        values.push(Value::Text(position.id.clone()));
    }
    sql.push_str(
        " ORDER BY CAST(session_created_at AS INTEGER) DESC, observed_at ASC,
                  CAST(seq AS INTEGER) ASC, message_id ASC LIMIT ?) page ON 1 = 1
          ORDER BY CAST(page.session_created_at AS INTEGER) DESC, page.observed_at ASC,
                   CAST(page.seq AS INTEGER) ASC, page.message_id ASC",
    );
    values.push(Value::Integer(fetch_limit as i64));
    let mut statement = conn.prepare(&sql)?;
    let page = statement
        .query_map(params_from_iter(values.iter()), |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let total = page.first().map(|(_, total)| *total).unwrap_or(0);
    let ids = page.into_iter().filter_map(|(id, _)| id).collect::<Vec<_>>();
    let (mut rows, missing) = hydrate_search_rows(conn, &ids)?;
    assign_source_ids(conn, &mut rows)?;
    Ok((rows, u64::try_from(total).unwrap_or(0), missing))
}

fn match_mode_clause(text: &str, mode: MatchMode) -> (&'static str, String) {
    match mode {
        MatchMode::Fts => ("history_message_fts MATCH ?", fts_phrase(text)),
        MatchMode::Like => (
            "body LIKE ? ESCAPE '\\'",
            format!("%{}%", escape_like(&text.to_lowercase())),
        ),
    }
}

fn hydrate_search_rows(
    conn: &Connection,
    ids: &[String],
) -> Result<(Vec<SearchRow>, usize), AppError> {
    if ids.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let placeholders = std::iter::repeat("?")
        .take(ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT m.id, m.session_id, m.run_id, m.native_event_id, m.source_key, m.seq, m.kind,
                m.content_json, m.source_ref, m.observed_at,
                s.title, s.agent_id, s.created_at_ms,
                (SELECT COUNT(*) FROM messages mm WHERE mm.session_id = m.session_id)
         FROM messages m JOIN sessions s ON s.id = m.session_id
         WHERE m.id IN ({placeholders})"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids.iter().map(String::as_str)), |row| {
        let message = map_search_message(row)?;
        Ok(SearchRow {
            message,
            session_title: row.get(10)?,
            agent_id: row.get(11)?,
            session_created_at: row.get(12)?,
            session_message_count: u64::try_from(row.get::<_, i64>(13)?).unwrap_or(0),
            source_ids: Vec::new(),
        })
    })?;
    let mut by_id = std::collections::HashMap::new();
    for row in rows {
        let row = row?;
        by_id.insert(row.message.id.clone(), row);
    }
    let mut out = Vec::with_capacity(ids.len());
    let mut missing = 0_usize;
    for id in ids {
        match by_id.remove(id) {
            Some(row) => out.push(row),
            None => missing += 1,
        }
    }
    Ok((out, missing))
}

fn assign_source_ids(conn: &Connection, rows: &mut [SearchRow]) -> Result<(), AppError> {
    if rows.is_empty() {
        return Ok(());
    }
    let placeholders = std::iter::repeat("?")
        .take(rows.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT message_id, source_id FROM history_message_origins
         WHERE message_id IN ({placeholders}) ORDER BY source_id"
    );
    let mut statement = conn.prepare(&sql)?;
    let pairs = statement.query_map(
        params_from_iter(rows.iter().map(|row| row.message.id.as_str())),
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    let mut by_message: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for pair in pairs {
        let (message_id, source_id) = pair?;
        by_message.entry(message_id).or_default().push(source_id);
    }
    for row in rows.iter_mut() {
        row.source_ids = by_message.remove(&row.message.id).unwrap_or_default();
    }
    Ok(())
}

fn search_where(
    text: &str,
    filter: &SearchFilter<'_>,
    mode: MatchMode,
) -> (String, Vec<Value>) {
    let mut sql = String::from(" WHERE ");
    let mut values = Vec::new();
    match mode {
        MatchMode::Fts => {
            sql.push_str("history_message_fts MATCH ?");
            values.push(Value::Text(fts_phrase(text)));
        }
        MatchMode::Like => {
            sql.push_str("history_message_fts.body LIKE ? ESCAPE '\\'");
            values.push(Value::Text(format!(
                "%{}%",
                escape_like(&text.to_lowercase())
            )));
        }
    }
    if let Some(agent_id) = filter.agent_id {
        sql.push_str(" AND s.agent_id = ?");
        values.push(Value::Text(agent_id.to_string()));
    }
    if let Some(workspace_id) = filter.workspace_id {
        sql.push_str(
            " AND EXISTS (
                 SELECT 1 FROM session_workspaces sw
                 WHERE sw.session_id = m.session_id AND sw.workspace_id = ?
             )",
        );
        values.push(Value::Text(workspace_id.to_string()));
    }
    if let Some(source_id) = filter.source_id {
        sql.push_str(
            " AND EXISTS (
                 SELECT 1 FROM history_message_origins o
                 WHERE o.message_id = m.id AND o.source_id = ?
             )",
        );
        values.push(Value::Text(source_id.to_string()));
    }
    (sql, values)
}

fn map_search_message(row: &Row<'_>) -> rusqlite::Result<Message> {
    let content_json: String = row.get(7)?;
    let content: JsonValue = serde_json::from_str(&content_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            Box::new(AppError::new(
                ErrorCode::Database,
                format!("stored message content is not valid JSON: {error}"),
            )),
        )
    })?;
    Ok(Message {
        id: row.get(0)?,
        session_id: row.get(1)?,
        run_id: row.get(2)?,
        native_event_id: row.get(3)?,
        source_key: row.get(4)?,
        seq: u64::try_from(row.get::<_, i64>(5)?).unwrap_or(0),
        kind: row.get(6)?,
        content,
        source_ref: row.get(8)?,
        observed_at: row.get(9)?,
    })
}

/// Whether a session has any message from a history source at all.
pub fn session_is_history(conn: &Connection, session_id: &str) -> Result<bool, AppError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM history_session_facts WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub fn fact_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<(String, String, String)>, AppError> {
    Ok(conn
        .query_row(
            "SELECT source_id, native_id, title_source FROM history_session_facts
             WHERE session_id = ?1 ORDER BY created_at_ms LIMIT 1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?)
}
