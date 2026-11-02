//! Developer tooling for the explicit fixture/probe/benchmark binaries.
//!
//! These helpers are **not** an IPC surface: they are not registered in
//! `multicli_bootstrap` and take an explicit root. They exist because the
//! examples are separate crates and cannot reach the crate-private store
//! connection; keeping one narrow, documented door is safer than exposing the
//! SQLite handle to every caller.

use super::model::product_for_key;
use super::repo::{self, SourceRow};
use super::scan::{scan_source, ScanInput, ScanOptions, ScanReport};
use crate::multicli::error::AppError;
use crate::multicli::MultiCliService;
use std::path::Path;

/// Registers (or reuses) one source at an explicit directory and creates its
/// index state. The role of the path is validated with the same boundary the
/// product command uses.
pub fn register_source(
    service: &MultiCliService,
    product_key: &str,
    root: &Path,
    config_profile_id: &str,
    enabled: bool,
) -> Result<SourceRow, AppError> {
    let product = product_for_key(product_key).ok_or_else(|| {
        AppError::invalid_argument("product", "must be a supported history product")
    })?;
    let (display, canonical) =
        super::query::validate_source_root(&root.to_string_lossy())?;
    super::query::ensure_agent_for_product(service, product.product)?;
    let conn = service.store().connection().lock();
    let source = repo::upsert_source(
        &conn,
        "local-windows",
        product,
        None,
        config_profile_id,
        &display,
        &canonical,
        enabled,
        None,
    )?;
    repo::ensure_state(&conn, &source.id)?;
    Ok(source)
}

/// Runs one bounded pass with the production scanner.
pub fn run_scan(
    service: &MultiCliService,
    source: &SourceRow,
    options: ScanOptions,
) -> Result<ScanReport, AppError> {
    scan_source(
        ScanInput {
            service,
            source,
            options,
            cancel: &|| false,
        },
        |_| {},
    )
}

/// (messages, cursors, accumulated row errors) for one source.
pub fn source_counts(
    service: &MultiCliService,
    source_id: &str,
) -> Result<(i64, i64, i64), AppError> {
    let conn = service.store().connection().lock();
    // A message can be contributed by more than one file of the same source,
    // so counting origin rows would overstate the imported message count.
    let messages: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT message_id) FROM history_message_origins WHERE source_id = ?1",
        [source_id],
        |row| row.get(0),
    )?;
    let cursors: i64 = conn.query_row(
        "SELECT COUNT(*) FROM history_cursors WHERE source_id = ?1",
        [source_id],
        |row| row.get(0),
    )?;
    let errors: i64 = conn.query_row(
        "SELECT COALESCE(SUM(error_count), 0) FROM history_cursors WHERE source_id = ?1",
        [source_id],
        |row| row.get(0),
    )?;
    Ok((messages, cursors, errors))
}

pub fn history_session_count(service: &MultiCliService) -> Result<i64, AppError> {
    let conn = service.store().connection().lock();
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM sessions WHERE origin = 'history-only'",
        [],
        |row| row.get(0),
    )?)
}

/// Stable digest of the whole message timeline (id, seq, observedAt, body
/// length, in order). The probe uses it to prove a restart does not rewrite or
/// reorder stored rows; counting rows alone would not catch either.
pub fn message_digest(service: &MultiCliService) -> Result<String, AppError> {
    let conn = service.store().connection().lock();
    let mut statement = conn.prepare(
        "SELECT id, seq, observed_at, length(content_json) FROM messages
         ORDER BY observed_at, seq, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(format!(
            "{}|{}|{}|{}
",
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?
        ))
    })?;
    let mut input = String::new();
    for row in rows {
        input.push_str(&row?);
    }
    Ok(super::ids::sha256_hex(input.as_bytes()))
}

pub fn message_count(service: &MultiCliService) -> Result<i64, AppError> {
    let conn = service.store().connection().lock();
    Ok(conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?)
}
