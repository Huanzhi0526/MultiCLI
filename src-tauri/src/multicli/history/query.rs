//! Read models for the history commands.
//!
//! Every list is bounded, every cursor is bound to one list, and every failure
//! stays an `AppError`. A missing source or session is `not_found`, never an
//! empty page.

use super::model::{
    HistoryCandidateView, HistorySourceView, HistoryStatusView, MessageKindCount, MessagePage,
    MessagePageQuery, SessionMetadataView, SessionWorkspaceDetail, HistoryProvenanceView,
    HistorySearchQuery, SearchHitView, SearchPage, METADATA_NOTE, SEARCH_NOTE,
    STATUS_NOTE,
};
use super::repo::{self, MatchMode};
use super::search;
use crate::multicli::domain::{AgentDefinition, HostPath, Page, PageRequest};
use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::history::model::{
    decode_message_cursor, encode_message_cursor, product_for_agent, PRODUCTS,
};
use crate::multicli::repo as domain_repo;
use crate::multicli::MultiCliService;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceCursor {
    sort: String,
    key: String,
    id: String,
}

pub fn list_sources_page(
    service: &MultiCliService,
    request: &PageRequest,
) -> Result<Page<HistorySourceView>, AppError> {
    request.validate()?;
    let cursor = request
        .cursor
        .as_deref()
        .map(decode_source_cursor)
        .transpose()?;
    let conn = service.store().connection().lock();
    let sources = repo::list_sources(&conn)?;
    let limit = request.limit as usize;
    let mut rows: Vec<(String, HistorySourceView)> = Vec::new();
    for source in sources {
        let key = format!("{}\u{1f}{}", source.agent_id, source.root_display_path);
        if let Some((cursor_key, cursor_id)) = &cursor {
            let after = &key > cursor_key || (&key == cursor_key && &source.id > cursor_id);
            if !after {
                continue;
            }
        }
        let state = repo::read_state(&conn, &source.id)?;
        rows.push((source.id.clone(), source_view(&source, state.as_ref())));
    }
    let total = count_sources(&conn)?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if has_more {
        rows.last().map(|(_, view)| {
            let key = format!("{}\u{1f}{}", view.agent_id, view.root.display_path);
            encode_source_cursor(&key, &view.source_id)
        })
    } else {
        None
    };
    Ok(Page {
        items: rows.into_iter().map(|(_, view)| view).collect(),
        next_cursor,
        total,
    })
}

fn count_sources(conn: &rusqlite::Connection) -> Result<u64, AppError> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM history_sources", [], |row| row.get(0))?;
    Ok(u64::try_from(count).unwrap_or(0))
}

fn decode_source_cursor(cursor: &str) -> Result<(String, String), AppError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| AppError::invalid_argument("cursor", "is not a valid pagination cursor"))?;
    let payload: SourceCursor = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::invalid_argument("cursor", "is not a valid pagination cursor"))?;
    if payload.sort != "history-sources" {
        return Err(AppError::invalid_argument(
            "cursor",
            "belongs to a different list",
        ));
    }
    Ok((payload.key, payload.id))
}

fn encode_source_cursor(key: &str, id: &str) -> String {
    let payload = SourceCursor {
        sort: "history-sources".to_string(),
        key: key.to_string(),
        id: id.to_string(),
    };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap_or_default())
}

pub fn source_view(
    source: &repo::SourceRow,
    state: Option<&repo::IndexStateRow>,
) -> HistorySourceView {
    HistorySourceView {
        source_id: source.id.clone(),
        host_id: source.host_id.clone(),
        agent_id: source.agent_id.clone(),
        product: source.product.clone(),
        installation_id: source.installation_id.clone(),
        config_profile_id: source.config_profile_id.clone(),
        config_profile_status: "unconfirmed".to_string(),
        root: source.root(),
        enabled: source.enabled,
        parser_version: source.parser_version,
        created_at: super::ids::iso_from_millis(source.created_at_ms),
        updated_at: super::ids::iso_from_millis(source.updated_at_ms),
        status: status_view(state, source.parser_version),
    }
}

pub fn status_view(
    state: Option<&repo::IndexStateRow>,
    parser_version: u32,
) -> HistoryStatusView {
    let Some(state) = state else {
        return HistoryStatusView::idle();
    };
    let error = state
        .error_json
        .as_deref()
        .and_then(|text| serde_json::from_str::<AppError>(text).ok());
    let processed = state
        .files_done
        .saturating_add(state.files_skipped)
        .saturating_add(state.files_failed);
    let progress_permille = if state.files_total == 0 {
        if state.status == "idle" {
            0
        } else {
            1000
        }
    } else {
        ((processed.saturating_mul(1000)) / state.files_total).min(1000) as u32
    };
    HistoryStatusView {
        generation: state.generation,
        status: state.status.clone(),
        in_flight: state.in_flight,
        last_attempt_at: state.last_attempt_at.clone(),
        last_success_at: state.last_success_at.clone(),
        error,
        files_total: state.files_total,
        files_done: state.files_done,
        files_skipped: state.files_skipped,
        files_failed: state.files_failed,
        messages_indexed: state.messages_indexed,
        bytes_read: state.bytes_read,
        complete: state.complete,
        progress_permille,
        note: state
            .note
            .clone()
            .unwrap_or_else(|| format!("{STATUS_NOTE} parserVersion {parser_version}.")),
    }
}

/// Known product roots under the user's home directory. This performs a
/// bounded top-level listing (at most 200 entries per candidate) and never
/// descends into a transcript tree or reads a file.
pub fn list_candidates(service: &MultiCliService) -> Result<Vec<HistoryCandidateView>, AppError> {
    let Some(home) = dirs::home_dir() else {
        return Ok(Vec::new());
    };
    let conn = service.store().connection().lock();
    let configured: Vec<String> = repo::list_sources(&conn)?
        .into_iter()
        .map(|source| super::ids::fold_path(&source.root_canonical_path))
        .collect();
    drop(conn);

    let mut out = Vec::new();
    for product in PRODUCTS {
        let mut root = home.clone();
        for component in product.home_components {
            root.push(component);
        }
        let display = root.to_string_lossy().to_string();
        let metadata = std::fs::metadata(&root);
        let exists = metadata.as_ref().map(|meta| meta.is_dir()).unwrap_or(false);
        let (readable, entry_sample) = if exists {
            match std::fs::read_dir(&root) {
                Ok(entries) => {
                    let mut count = 0_u64;
                    for entry in entries.take(200) {
                        if entry.is_ok() {
                            count += 1;
                        }
                    }
                    (true, count)
                }
                Err(_) => (false, 0),
            }
        } else {
            (false, 0)
        };
        let canonical = std::fs::canonicalize(&root).ok();
        let already_configured = canonical
            .as_ref()
            .map(|path| {
                let folded = super::ids::fold_path(&crate::multicli::path::canonical_text(path));
                configured.iter().any(|configured| configured == &folded)
            })
            .unwrap_or(false);
        out.push(HistoryCandidateView {
            agent_id: product.agent_id.to_string(),
            product: product.product.to_string(),
            display_name: product.display_name.to_string(),
            root: HostPath {
                host_id: "local-windows".to_string(),
                display_path: display,
                canonical_path: canonical
                    .as_ref()
                    .map(|path| crate::multicli::path::canonical_text(path)),
            },
            exists,
            readable,
            entry_sample,
            suggestion: "home-default".to_string(),
            already_configured,
            note: "A candidate is a directory that exists; it is not evidence that its records are readable or complete. Choosing it is a user action.".to_string(),
        });
    }
    Ok(out)
}

pub fn session_metadata(
    service: &MultiCliService,
    session_id: &str,
) -> Result<SessionMetadataView, AppError> {
    if session_id.trim().is_empty() {
        return Err(AppError::invalid_argument(
            "sessionId",
            "must not be empty",
        ));
    }
    let conn = service.store().connection().lock();
    let session = domain_repo::get_session(&conn, session_id)?
        .ok_or_else(|| AppError::not_found("session", session_id))?;
    let workspace_rows = domain_repo::session_workspace_rows(&conn, session_id)?;
    let workspaces = workspace_rows
        .into_iter()
        .map(|(link, workspace)| SessionWorkspaceDetail {
            workspace_id: link.workspace_id,
            role: link.role,
            evidence: link.evidence,
            path: workspace.map(|workspace| workspace.path),
        })
        .collect();
    let (runs, run_total) = domain_repo::runs_page_for_session(&conn, session_id, 50)?;
    let message_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )?;
    let message_kinds = domain_repo::message_kind_counts(&conn, session_id, 32)?
        .into_iter()
        .map(|(kind, count)| MessageKindCount { kind, count })
        .collect();
    let (first_message_at, last_message_at) =
        domain_repo::message_time_bounds(&conn, session_id)?;
    let (source_ids, file_count, parser_versions) =
        domain_repo::history_provenance(&conn, session_id)?;
    let history = if source_ids.is_empty() {
        None
    } else {
        Some(HistoryProvenanceView {
            source_ids,
            file_count,
            parser_versions,
            read_only: true,
        })
    };
    Ok(SessionMetadataView {
        session,
        workspaces,
        runs,
        run_total,
        message_count: u64::try_from(message_count).unwrap_or(0),
        message_kinds,
        first_message_at,
        last_message_at,
        history,
        note: METADATA_NOTE.to_string(),
    })
}

pub fn message_page(
    service: &MultiCliService,
    query: &MessagePageQuery,
) -> Result<MessagePage, AppError> {
    query.validate()?;
    let conn = service.store().connection().lock();
    if domain_repo::get_session(&conn, &query.session_id)?.is_none() {
        return Err(AppError::not_found("session", &query.session_id));
    }
    let limit = query.limit as usize;
    let mut anchor_index: Option<u32> = None;
    let epoch = repo::purge_epoch(&conn)?;
    let (cursor, offset) = if let Some(anchor) = &query.anchor_message_id {
        let Some((position, position_index)) =
            domain_repo::message_position(&conn, &query.session_id, anchor)?
        else {
            return Err(AppError::not_found("message", anchor));
        };
        // The window starts a quarter-page before the anchor so the message is
        // visible with context, then continues forward with `nextCursor`.
        let back = (limit / 4) as u64;
        let start = position_index.saturating_sub(back);
        anchor_index = Some((position_index - start) as u32);
        let _ = position;
        (None, Some(start))
    } else if let Some(raw) = &query.cursor {
        (Some(decode_message_cursor(raw, &query.session_id, epoch)?), None)
    } else {
        (None, None)
    };
    let (mut items, total) = domain_repo::session_messages_page(
        &conn,
        &query.session_id,
        cursor.as_ref(),
        offset,
        limit + 1,
    )?;
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if has_more {
        items.last().map(|message| {
            encode_message_cursor(
                &query.session_id,
                &message.observed_at,
                message.seq,
                &message.id,
                epoch,
            )
        })
    } else {
        None
    };
    Ok(MessagePage {
        items,
        next_cursor,
        total,
        anchor_message_id: query.anchor_message_id.clone(),
        anchor_index,
    })
}

pub fn search_history(
    service: &MultiCliService,
    query: &HistorySearchQuery,
) -> Result<SearchPage, AppError> {
    query.validate()?;
    let text = query.normalized_text();
    let mode = super::repo::match_mode_for(&text);
    let limit = query.limit as usize;
    let conn = service.store().connection().lock();
    let binding = super::model::SearchBinding::new(
        &text,
        query.workspace_id.as_deref(),
        query.agent_id.as_deref(),
        query.source_id.as_deref(),
        mode_str(mode),
        repo::purge_epoch(&conn)?,
    );
    let cursor = query
        .cursor
        .as_deref()
        .map(|raw| super::model::decode_search_cursor(raw, &binding))
        .transpose()?;
    let filter = repo::SearchFilter {
        workspace_id: query.workspace_id.as_deref(),
        agent_id: query.agent_id.as_deref(),
        source_id: query.source_id.as_deref(),
    };
    let (rows, total) = repo::search_messages(&conn, &text, &filter, mode, cursor.as_ref(), limit + 1)?;
    let has_more = rows.len() > limit;
    let rows = &rows[..rows.len().min(limit)];
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let body = search::body_text(&row.message.content);
        let (snippet, snippet_truncated) = search::snippet(&body, &text, 60, 120);
        items.push(SearchHitView {
            session_id: row.message.session_id.clone(),
            message_id: row.message.id.clone(),
            session_title: row.session_title.clone(),
            agent_id: row.agent_id.clone(),
            kind: row.message.kind.clone(),
            seq: row.message.seq,
            observed_at: row.message.observed_at.clone(),
            snippet,
            snippet_truncated,
            match_mode: mode_str(mode).to_string(),
            source_ids: row.source_ids.clone(),
            session_message_count: row.session_message_count,
        });
    }
    let next_cursor = if has_more {
        rows.last().map(|row| {
            super::model::encode_search_cursor(
                &super::model::SearchPosition {
                    session_created_at: row.session_created_at,
                    observed_at: row.message.observed_at.clone(),
                    seq: row.message.seq,
                    id: row.message.id.clone(),
                },
                &binding,
            )
        })
    } else {
        None
    };
    Ok(SearchPage {
        items,
        next_cursor,
        total,
        match_mode: mode_str(mode).to_string(),
        note: SEARCH_NOTE.to_string(),
    })
}

fn mode_str(mode: MatchMode) -> &'static str {
    match mode {
        MatchMode::Fts => "fts",
        MatchMode::Like => "like",
    }
}

/// Validates a history root before it is stored.
///
/// This is a real boundary, not a formatting check: a whole drive, a user
/// profile root, a credential directory or a system directory is rejected even
/// when the user typed it. Sub-directories of a credential directory are
/// rejected too, because the check runs over every component.
pub fn validate_source_root(raw: &str) -> Result<(String, String), AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid_argument("root", "must not be empty"));
    }
    let normalized = crate::multicli::path::normalize_display_path(trimmed)
        .map_err(|_| AppError::invalid_argument("root", "is not a valid path"))?;
    let path = Path::new(&normalized);
    if !path.is_absolute() {
        return Err(AppError::invalid_argument("root", "must be absolute"));
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::AccessDenied,
            _ => ErrorCode::Io,
        };
        AppError::new(code, "the history root is not accessible")
            .with_details(serde_json::json!({ "stage": "historyRoot", "field": "root" }))
            .with_retryable(matches!(code, ErrorCode::Io))
    })?;
    if !metadata.is_dir() {
        return Err(AppError::invalid_argument("root", "must be a directory"));
    }
    // Existence is not readability: a directory can exist and still deny
    // listing (ACLs, an offline share). Probe one entry now so the user gets
    // access_denied at configuration time instead of an unexplained empty pass
    // later.
    let mut probe = std::fs::read_dir(path).map_err(|error| {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::AccessDenied,
            _ => ErrorCode::Io,
        };
        AppError::new(code, "the history root cannot be listed")
            .with_details(serde_json::json!({ "stage": "historyRoot", "field": "root" }))
            .with_retryable(matches!(code, ErrorCode::Io))
    })?;
    if let Some(Err(error)) = probe.next() {
        let code = match error.kind() {
            std::io::ErrorKind::PermissionDenied => ErrorCode::AccessDenied,
            _ => ErrorCode::Io,
        };
        return Err(AppError::new(code, "the history root cannot be listed")
            .with_details(serde_json::json!({ "stage": "historyRoot", "field": "root" }))
            .with_retryable(matches!(code, ErrorCode::Io)));
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        AppError::new(ErrorCode::Io, "the history root could not be resolved")
            .with_details(serde_json::json!({
                "stage": "historyRoot",
                "reason": error.kind().to_string(),
            }))
    })?;
    let canonical_text = crate::multicli::path::canonical_text(&canonical);
    let components: Vec<String> = Path::new(&canonical_text)
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    if components.len() < 2 {
        return Err(rejected_root("drive-root"));
    }
    // One policy, shared with the recursive walk: the old local list had
    // drifted (missing `.password-store`, no trailing-dot normalisation, and a
    // `microsoft\\credentials` entry that could never match a single component).
    if super::guard::protected_in(&components).is_some() {
        return Err(rejected_root("protected-directory"));
    }
    let folded = super::ids::fold_path(&canonical_text);
    if let Some(home) = dirs::home_dir() {
        let home_text = crate::multicli::path::canonical_text(&home);
        if super::ids::fold_path(&home_text) == folded {
            return Err(rejected_root("user-profile-root"));
        }
    }
    if let Some(app_data) = dirs::data_dir() {
        let text = super::ids::fold_path(&crate::multicli::path::canonical_text(&app_data));
        if folded == text {
            return Err(rejected_root("app-data-root"));
        }
    }
    Ok((normalized, canonical_text))
}

fn rejected_root(reason: &str) -> AppError {
    AppError::new(
        ErrorCode::InvalidArgument,
        "this directory is outside the allowed history roots",
    )
    .with_details(serde_json::json!({
        "stage": "historyRoot",
        "field": "root",
        "reason": reason,
    }))
}

/// Small helper used by the engine when it needs an agent row to exist before a
/// source references it. It never fabricates an installation.
pub fn ensure_agent_for_product(
    service: &MultiCliService,
    product_key: &str,
) -> Result<AgentDefinition, AppError> {
    let product = product_for_agent(product_key)
        .or_else(|| super::model::product_for_key(product_key))
        .ok_or_else(|| {
            AppError::invalid_argument("product", "must be a supported history product")
        })?;
    let agent = AgentDefinition {
        id: product.agent_id.to_string(),
        vendor: match product.agent_id {
            "codex" => "OpenAI",
            "claude" => "Anthropic",
            _ => "Tencent",
        }
        .to_string(),
        display_name: product.display_name.to_string(),
    };
    // Insert only when the agent is missing. History must not rewrite the
    // identity S02 discovered (vendor/product name come from the executable and
    // wrapper there); a blind upsert would overwrite them on every source edit.
    let conn = service.store().connection().lock();
    conn.execute(
        "INSERT INTO agents(id, vendor, display_name, created_at_ms)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO NOTHING",
        rusqlite::params![
            agent.id.as_str(),
            agent.vendor.as_str(),
            agent.display_name.as_str(),
            crate::multicli::store::now_ms(),
        ],
    )?;
    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_roots_are_rejected() {
        let root = std::env::temp_dir().join(format!(
            "multicli-s03-roots-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let ssh = root.join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        let nested = ssh.join("keys");
        std::fs::create_dir_all(&nested).unwrap();
        // A credential directory is rejected by name, including below it.
        for candidate in [&ssh, &nested] {
            let error = validate_source_root(&candidate.to_string_lossy()).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{candidate:?}");
        }
        assert_eq!(
            validate_source_root("relative\\path").unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            validate_source_root(&root.join("does-not-exist").to_string_lossy())
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        // The user profile root is never a history root.
        if let Some(home) = dirs::home_dir() {
            assert_eq!(
                validate_source_root(&home.to_string_lossy()).unwrap_err().code,
                ErrorCode::InvalidArgument
            );
        }
        // A normal directory is accepted and canonicalized.
        let accepted = root.join("history");
        std::fs::create_dir_all(&accepted).unwrap();
        let (display, canonical) = validate_source_root(&accepted.to_string_lossy()).unwrap();
        assert!(!display.is_empty());
        assert!(canonical.to_ascii_lowercase().contains("history"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn source_cursor_round_trips() {
        let cursor = encode_source_cursor("codex\u{1f}C:\\x", "hsrc-1");
        assert_eq!(
            decode_source_cursor(&cursor).unwrap(),
            ("codex\u{1f}C:\\x".to_string(), "hsrc-1".to_string())
        );
        assert!(decode_source_cursor("nope").is_err());
        assert!(decode_source_cursor(&encode_message_cursor("s", "t", 1, "m", 0)).is_err());
    }
}
