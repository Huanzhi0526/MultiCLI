//! Read-only history import probe (developer tool, not a product command).
//!
//! It points the indexer at explicit directories, imports them twice, hashes
//! the source trees before and after, then reopens the database and compares
//! the whole message timeline. It never writes to a source.
//!
//! Usage:
//!   cargo run --locked --example multicli_history_probe -- \
//!     --root D:\scratch\s03\data \
//!     --source codex=D:\scratch\s03\copies\codex \
//!     --source claude-code=D:\scratch\s03\copies\claude \
//!     --source codebuddy-code=D:\scratch\s03\copies\codebuddy
//!
//! The root must be an explicit scratch directory. Nothing outside it is
//! written, and no CLI data directory is opened for writing.

use ccgui_next_lib::multicli::history::ids;
use ccgui_next_lib::multicli::history::model::{
    product_for_key, HistorySearchQuery, HISTORY_PARSER_VERSION,
};
use ccgui_next_lib::multicli::history::query;
use ccgui_next_lib::multicli::history::scan::ScanOptions;
use ccgui_next_lib::multicli::history::tooling;
use ccgui_next_lib::multicli::{Host, MultiCliService};
use serde_json::json;
use std::path::{Path, PathBuf};

struct Args {
    root: PathBuf,
    sources: Vec<(String, PathBuf)>,
    searches: Vec<String>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("multicli_history_probe: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if !args.root.is_absolute() {
        return Err("--root must be absolute".into());
    }
    std::fs::create_dir_all(&args.root).map_err(|error| error.to_string())?;
    let service = MultiCliService::open(&args.root).map_err(|error| error.to_string())?;
    service
        .register_host(&Host::local("local-windows"))
        .map_err(|error| error.to_string())?;

    let mut results = Vec::new();
    let mut hash_reports = Vec::new();
    let mut total_failed = 0_u64;
    for (product_key, source_root) in &args.sources {
        let product = product_for_key(product_key)
            .ok_or_else(|| format!("unsupported product {product_key}"))?;
        let before = hash_tree(source_root, product.extensions)?;
        let source = tooling::register_source(&service, product_key, source_root, "probe", true)
            .map_err(|error| error.to_string())?;
        let first = tooling::run_scan(&service, &source, ScanOptions::incremental())
            .map_err(|error| error.to_string())?;
        let second = tooling::run_scan(&service, &source, ScanOptions::incremental())
            .map_err(|error| error.to_string())?;
        let after = hash_tree(source_root, product.extensions)?;
        let (messages, cursors, row_errors) =
            tooling::source_counts(&service, &source.id).map_err(|error| error.to_string())?;
        total_failed += first.progress.files_failed + second.progress.files_failed;

        let mismatches: Vec<String> = before
            .iter()
            .zip(after.iter())
            .filter(|(left, right)| left != right)
            .map(|(left, _)| left.0.clone())
            .take(5)
            .collect();
        let unchanged = before.len() == after.len() && mismatches.is_empty();
        hash_reports.push(json!({
            "product": product.product,
            "files": before.len(),
            "unchanged": unchanged,
            "mismatches": mismatches,
        }));

        results.push(json!({
            "product": source.product,
            "agentId": source.agent_id,
            "sourceId": source.id,
            "parserVersion": HISTORY_PARSER_VERSION,
            "firstPass": {
                "listed": first.listed_files,
                "done": first.progress.files_done,
                "skipped": first.progress.files_skipped,
                "failed": first.progress.files_failed,
                "messages": first.progress.messages,
                "complete": first.complete,
                "issues": first.issues.iter().take(8).map(|issue| json!({
                    "code": issue.code,
                    "fileIndex": issue.file_index,
                    "line": issue.line,
                    "detail": issue.detail,
                })).collect::<Vec<_>>(),
            },
            "secondPass": {
                "messages": second.progress.messages,
                "skipped": second.progress.files_skipped,
                "failed": second.progress.files_failed,
                "complete": second.complete,
            },
            "storedMessages": messages,
            "cursors": cursors,
            "rowErrors": row_errors,
            "sourceHash": {
                "files": before.len(),
                "unchanged": unchanged,
            },
        }));
    }

    let mut searches = Vec::new();
    for text in &args.searches {
        let page = query::search_history(
            &service,
            &HistorySearchQuery {
                text: text.clone(),
                workspace_id: None,
                agent_id: None,
                source_id: None,
                limit: 10,
                cursor: None,
            },
        )
        .map_err(|error| error.to_string())?;
        searches.push(json!({
            "text": text,
            "total": page.total,
            "matchMode": page.match_mode,
            "hits": page.items.iter().map(|hit| json!({
                "sessionId": hit.session_id,
                "messageId": hit.message_id,
                "agentId": hit.agent_id,
                "kind": hit.kind,
                "snippet": hit.snippet,
                "snippetTruncated": hit.snippet_truncated,
                "sourceIds": hit.source_ids,
            })).collect::<Vec<_>>(),
        }));
    }

    let sessions = tooling::history_session_count(&service).map_err(|error| error.to_string())?;
    let messages_before = tooling::message_count(&service).map_err(|error| error.to_string())?;
    let digest_before = tooling::message_digest(&service).map_err(|error| error.to_string())?;
    let database = service.database_path().display().to_string();
    let schema_version = service.schema_version();
    drop(service);

    // Reopen over the same data root: a restart must keep the exact timeline.
    let reopened = MultiCliService::open(&args.root).map_err(|error| error.to_string())?;
    let messages_after = tooling::message_count(&reopened).map_err(|error| error.to_string())?;
    let digest_after = tooling::message_digest(&reopened).map_err(|error| error.to_string())?;

    let output = json!({
        "dataRoot": args.root.display().to_string(),
        "database": database,
        "schemaVersion": schema_version,
        "historyOnlySessions": sessions,
        "storedMessages": messages_before,
        "filesFailed": total_failed,
        "sources": results,
        "sourceHashes": hash_reports,
        "searches": searches,
        "restart": {
            "messagesBefore": messages_before,
            "messagesAfter": messages_after,
            "timelineDigest": digest_before,
            "timelineMatches": digest_before == digest_after,
        },
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output).map_err(|error| error.to_string())?
    );
    if total_failed > 0 {
        return Err(format!("{total_failed} file(s) could not be imported"));
    }
    if digest_before != digest_after {
        return Err("the message timeline changed across restart".into());
    }
    Ok(())
}

/// One digest per transcript file, plus the count. Only the relative name, size
/// and content hash are retained; the probe never needs the absolute path.
fn hash_tree(root: &Path, extensions: &[&str]) -> Result<Vec<(String, u64, String)>, String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0_usize)];
    while let Some((directory, depth)) = stack.pop() {
        if depth > 6 || out.len() >= 2_000 {
            continue;
        }
        let entries = std::fs::read_dir(&directory).map_err(|error| error.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let metadata = entry.metadata().map_err(|error| error.to_string())?;
            if metadata.is_dir() {
                stack.push((path, depth + 1));
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !extensions
                .iter()
                .any(|extension| name.ends_with(&format!(".{extension}")))
            {
                continue;
            }
            let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            out.push((relative, bytes.len() as u64, ids::sha256_hex(&bytes)));
            if out.len() >= 2_000 {
                break;
            }
        }
    }
    out.sort();
    Ok(out)
}

fn parse_args() -> Result<Args, String> {
    let mut root: Option<PathBuf> = None;
    let mut sources = Vec::new();
    let mut searches = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                root = Some(PathBuf::from(args.next().ok_or("--root needs a value")?));
            }
            "--source" => {
                let value = args.next().ok_or("--source needs <product>=<path>")?;
                let (product, path) = value
                    .split_once('=')
                    .ok_or("--source must be <product>=<path>")?;
                if product.trim().is_empty() || path.trim().is_empty() {
                    return Err("--source must be <product>=<path>".into());
                }
                sources.push((product.trim().to_string(), PathBuf::from(path.trim())));
            }
            "--search" => {
                searches.push(args.next().ok_or("--search needs a value")?);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if sources.is_empty() {
        return Err("at least one --source is required".into());
    }
    Ok(Args {
        root: root.ok_or("--root is required")?,
        sources,
        searches,
    })
}
