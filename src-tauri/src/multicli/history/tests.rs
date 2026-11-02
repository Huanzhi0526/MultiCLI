//! S03 history tests.
//!
//! Every fixture is synthetic: no real user transcript, real database, CLI
//! configuration or credentials are read. The real-copy and performance evidence
//! lives in `scratch/S03` and is produced by the `multicli_history_probe` /
//! `multicli_history_bench` examples.

use super::engine::HistoryEngine;
use super::ids;
use super::model::{
    self, HistoryRebuildQuery, HistorySearchQuery, HistorySourceUpdate, MessagePageQuery,
};
use super::query;
use super::repo::{self, CursorRow, SourceRow};
use super::resume;
use super::scan::{scan_source, ScanInput, ScanOptions, ScanReport};
use crate::multicli::store::now_ms;
use crate::multicli::domain::{
    AgentDefinition, AgentInstallation, ExecutionStatus, Host, HostPath, InteractionStatus, Run,
    RunOwnership, RunTransport, Session, SessionOrigin, Workspace,
};
use crate::multicli::error::ErrorCode;
use crate::multicli::service::MultiCliService;
use crate::multicli::store::{MigrationFault, SCHEMA_VERSION};
use rusqlite::{params, Connection};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "multicli-s03-{name}-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&path).expect("create synthetic temp root");
        Self { path }
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// `service` is declared before `root`, so the SQLite connection closes before
/// Windows tries to remove the directory.
struct Fixture {
    service: MultiCliService,
    root: TempRoot,
    history: PathBuf,
    workspace: PathBuf,
    workspace_two: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = TempRoot::new(name);
        let history = root.path.join("history-root");
        let workspace = root.path.join("ws-one");
        let workspace_two = root.path.join("ws-two");
        fs::create_dir_all(&history).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&workspace_two).unwrap();
        let service = MultiCliService::open(root.path.join("data")).unwrap();
        service
            .register_host(&Host::local("local-windows"))
            .unwrap();
        Self {
            service,
            root,
            history,
            workspace,
            workspace_two,
        }
    }

    fn add_source(&self, product_key: &str) -> SourceRow {
        let product = model::product_for_agent(product_key).unwrap();
        self.service
            .register_agent(&AgentDefinition {
                id: product.agent_id.to_string(),
                vendor: "synthetic".to_string(),
                display_name: product.display_name.to_string(),
            })
            .unwrap();
        let conn = self.service.store().connection().lock();
        let source = repo::upsert_source(
            &conn,
            "local-windows",
            product,
            None,
            "unconfirmed-local",
            &self.history.to_string_lossy(),
            &crate::multicli::path::canonical_text(&fs::canonicalize(&self.history).unwrap()),
            true,
            None,
        )
        .unwrap();
        repo::ensure_state(&conn, &source.id).unwrap();
        source
    }

    fn add_source_at(&self, product_key: &str, root: &std::path::Path) -> SourceRow {
        let product = model::product_for_agent(product_key).unwrap();
        self.service
            .register_agent(&AgentDefinition {
                id: product.agent_id.to_string(),
                vendor: "synthetic".to_string(),
                display_name: product.display_name.to_string(),
            })
            .unwrap();
        let conn = self.service.store().connection().lock();
        let source = repo::upsert_source(
            &conn,
            "local-windows",
            product,
            None,
            "unconfirmed-local",
            &root.to_string_lossy(),
            &crate::multicli::path::canonical_text(&fs::canonicalize(root).unwrap()),
            true,
            None,
        )
        .unwrap();
        repo::ensure_state(&conn, &source.id).unwrap();
        source
    }

    fn scan(&self, source: &SourceRow) -> ScanReport {
        scan(&self.service, source, ScanOptions::incremental())
    }

    fn scan_with(&self, source: &SourceRow, options: ScanOptions) -> ScanReport {
        scan(&self.service, source, options)
    }

    fn workspace_path(&self) -> String {
        crate::multicli::path::canonical_text(&fs::canonicalize(&self.workspace).unwrap())
    }

    fn workspace_two_path(&self) -> String {
        crate::multicli::path::canonical_text(&fs::canonicalize(&self.workspace_two).unwrap())
    }

    fn message_count(&self) -> i64 {
        let conn = self.service.store().connection().lock();
        conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap()
    }

    fn session_count(&self) -> i64 {
        let conn = self.service.store().connection().lock();
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap()
    }

    /// Opens a second service over the same data root and drops the first.
    /// This is what a process restart does; the persisted rows, not the
    /// connection, are what must stay identical.
    fn restart(&mut self) {
        let data = self.root.path.join("data");
        let fresh = MultiCliService::open(&data).unwrap();
        let old = std::mem::replace(&mut self.service, fresh);
        drop(old);
    }
}

fn scan(service: &MultiCliService, source: &SourceRow, options: ScanOptions) -> ScanReport {
    scan_source(
        ScanInput {
            service,
            source,
            options,
            cancel: &|| false,
        },
        |_| {},
    )
    .expect("scan succeeds")
}

fn codex_lines(fixture: &Fixture, session: &str, extra_root: bool) -> Vec<String> {
    let meta = json!({
        "ordinal": 0,
        "timestamp": "2026-11-01T10:00:00.000Z",
        "type": "session_meta",
        "payload": {
            "session_id": session,
            "id": session,
            "timestamp": "2026-11-01T10:00:00.000Z",
            "cwd": fixture.workspace_path(),
            "cli_version": "0.166.0",
            "runtime_workspace_roots": if extra_root {
                vec![fixture.workspace_two_path()]
            } else {
                Vec::new()
            },
            "auth": { "token": "SENTINEL-TOKEN" }
        }
    });
    vec![
        meta.to_string(),
        json!({
            "ordinal": 1,
            "timestamp": "2026-11-01T10:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "id": "msg-user-1",
                "role": "user",
                "content": [{ "type": "input_text", "text": "请检查历史记录与中文搜索" }]
            }
        })
        .to_string(),
        json!({
            "ordinal": 2,
            "timestamp": "2026-11-01T10:00:02.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "id": "msg-assistant-1",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "读取了 100% 的文件，包含 emoji 🎲" }]
            }
        })
        .to_string(),
        json!({
            "ordinal": 3,
            "timestamp": "2026-11-01T10:00:03.000Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "id": "fc-1",
                "name": "shell",
                "arguments": "{\"cmd\":\"dir\"}",
                "call_id": "call-1",
                "authorization": "SENTINEL-SECRET"
            }
        })
        .to_string(),
    ]
}

fn write_lines(path: &std::path::Path, lines: &[String]) {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    fs::write(path, text).unwrap();
}

// ---------------------------------------------------------------------------
// Parsers and formatting
// ---------------------------------------------------------------------------

#[test]
fn codex_parser_maps_only_whitelisted_fields() {
    let fixture = Fixture::new("codex-parser");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000001.jsonl",
    );
    write_lines(
        &path,
        &codex_lines(&fixture, "019a0000-0000-7000-8000-000000000001", true),
    );
    let source = fixture.add_source("codex");
    let report = fixture.scan(&source);
    assert_eq!(report.progress.files_failed, 0);
    assert_eq!(report.progress.messages, 3);
    assert_eq!(fixture.session_count(), 1);

    let conn = fixture.service.store().connection().lock();
    let kinds: Vec<String> = {
        let mut statement = conn
            .prepare("SELECT kind FROM messages ORDER BY seq")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(kinds, vec!["user-message", "assistant-message", "tool-call"]);
    let content: String = conn
        .query_row(
            "SELECT content_json FROM messages WHERE kind = 'user-message'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(content.contains("请检查历史记录与中文搜索"));
    assert!(content.contains("unknownFields"));
    let tool_content: String = conn
        .query_row(
            "SELECT content_json FROM messages WHERE kind = 'tool-call'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // Unknown fields are reported by name only; their values are never stored.
    assert!(tool_content.contains("authorization"));
    assert!(!tool_content.contains("SENTINEL-SECRET"));
    let everything: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages
             WHERE content_json LIKE '%SENTINEL%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(everything, 0, "no secret value may reach the store");
    let session: (String, String) = conn
        .query_row("SELECT origin, title FROM sessions", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(session.0, "history-only");
    assert!(session.1.contains("请检查历史记录"));
    // Two workspaces: the cwd is primary and the declared root is related.
    let roles: Vec<String> = {
        let mut statement = conn
            .prepare("SELECT role FROM session_workspaces ORDER BY role")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(roles, vec!["primary", "related"]);
    // No Run is fabricated.
    let runs: i64 = conn
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(runs, 0);
}

#[test]
fn claude_and_codebuddy_parsers_use_their_own_identity() {
    let fixture = Fixture::new("three-formats");
    let claude_root = fixture.history.join("claude-projects");
    let buddy_root = fixture.history.join("buddy-projects");
    fs::create_dir_all(&claude_root).unwrap();
    fs::create_dir_all(&buddy_root).unwrap();
    let claude_path = claude_root.join("4efbc651-6bad-46dc-9e76-450ba13919df.jsonl");
    write_lines(
        &claude_path,
        &[
            json!({ "type": "mode", "sessionId": "claude-native-1", "mode": "default" }).to_string(),
            json!({
                "type": "user",
                "uuid": "u-1",
                "sessionId": "claude-native-1",
                "cwd": fixture.workspace_path(),
                "timestamp": "2026-11-01T11:00:00.000Z",
                "version": "2.1.153",
                "isSidechain": false,
                "message": { "role": "user", "content": "claude 的中文记录" }
            })
            .to_string(),
            json!({
                "type": "assistant",
                "uuid": "a-1",
                "sessionId": "claude-native-1",
                "cwd": fixture.workspace_path(),
                "timestamp": "2026-11-01T11:00:01.000Z",
                "isSidechain": false,
                "message": {
                    "role": "assistant",
                    "content": [
                        { "type": "text", "text": "好的" },
                        { "type": "tool_use", "id": "t-1", "name": "Bash", "input": { "command": "ls" } }
                    ]
                }
            })
            .to_string(),
            json!({
                "type": "ai-title",
                "sessionId": "claude-native-1",
                "aiTitle": "Claude 原生标题"
            })
            .to_string(),
        ],
    );
    let buddy_path = buddy_root.join("01a18b22-5c25-76f6-a5ac-82e47ac07ec0.jsonl");
    write_lines(
        &buddy_path,
        &[
            json!({
                "type": "session-meta",
                "id": "buddy-native-1",
                "sessionId": "buddy-native-1",
                "timestamp": 1_762_000_000_000_i64,
                "meta": {}
            })
            .to_string(),
            json!({
                "type": "message",
                "id": "b-1",
                "sessionId": "buddy-native-1",
                "role": "user",
                "cwd": fixture.workspace_path(),
                "timestamp": 1_762_000_000_100_i64,
                "content": [{ "type": "text", "text": "buddy 的历史" }]
            })
            .to_string(),
            json!({
                "type": "function_call_result",
                "id": "b-2",
                "sessionId": "buddy-native-1",
                "callId": "c-1",
                "name": "read_file",
                "status": "completed",
                "timestamp": 1_762_000_000_200_i64,
                "output": { "type": "text", "text": "done" }
            })
            .to_string(),
        ],
    );

    let claude_source = fixture.add_source_at("claude", &claude_root);
    let buddy_source = fixture.add_source_at("codebuddy", &buddy_root);
    let first = fixture.scan(&claude_source);
    assert_eq!(first.progress.files_failed, 0, "{:?}", first.issues);
    let second = fixture.scan(&buddy_source);
    assert_eq!(second.progress.files_failed, 0, "{:?}", second.issues);

    let conn = fixture.service.store().connection().lock();
    let sessions: Vec<(String, String, String)> = {
        let mut statement = conn
            .prepare("SELECT agent_id, source_namespace, title FROM sessions ORDER BY agent_id")
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].0, "claude");
    assert!(sessions[0].1.contains("claude-code"));
    assert_eq!(sessions[0].2, "Claude 原生标题");
    assert_eq!(sessions[1].0, "codebuddy");
    assert_ne!(sessions[0].1, sessions[1].1);
    let kinds: Vec<(String, i64)> = {
        let mut statement = conn
            .prepare(
                "SELECT kind, COUNT(*) FROM messages
                 GROUP BY kind ORDER BY kind",
            )
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert!(kinds.contains(&("user-message".to_string(), 2)));
    assert!(kinds.contains(&("assistant-message".to_string(), 1)));
    assert!(kinds.contains(&("tool-call".to_string(), 1)));
    assert!(kinds.contains(&("tool-result".to_string(), 1)));
}

#[test]
fn unrecognised_files_are_reported_not_silently_skipped() {
    let fixture = Fixture::new("format-mismatch");
    let path = fixture.history.join("wrong.jsonl");
    write_lines(
        &path,
        &[json!({ "type": "message", "text": "not a known envelope" }).to_string()],
    );
    let source = fixture.add_source("codex");
    let report = fixture.scan(&source);
    assert_eq!(report.progress.files_failed, 1);
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].code, "unsupported");
    assert_eq!(fixture.message_count(), 0);
    let conn = fixture.service.store().connection().lock();
    let cursors: i64 = conn
        .query_row("SELECT COUNT(*) FROM history_cursors", [], |row| row.get(0))
        .unwrap();
    assert_eq!(cursors, 0, "an unreadable file must not advance a cursor");
}

// ---------------------------------------------------------------------------
// Incremental behaviour (A07)
// ---------------------------------------------------------------------------

#[test]
fn partial_tail_waits_for_completion_and_second_import_is_idempotent() {
    let fixture = Fixture::new("partial-tail");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000002.jsonl",
    );
    let lines = codex_lines(&fixture, "019a0000-0000-7000-8000-000000000002", false);
    let mut text = lines.join("\n");
    text.push('\n');
    text.push_str(
        "{\"ordinal\":4,\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"ms",
    );
    fs::write(&path, &text).unwrap();

    let source = fixture.add_source("codex");
    let first = fixture.scan(&source);
    assert_eq!(first.progress.messages, 3);
    let conn = fixture.service.store().connection().lock();
    let cursor: (i64, String) = conn
        .query_row(
            "SELECT committed_offset, tail_residue FROM history_cursors",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(cursor.0 > 0);
    assert!(cursor.1.contains("ordinal"));
    drop(conn);

    // Finish the record: the same pass must import exactly the new one.
    let mut complete = text.clone();
    complete.push_str(
        "g-tail\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"补齐的半行\"}]}}\n",
    );
    fs::write(&path, &complete).unwrap();
    let second = fixture.scan(&source);
    assert_eq!(second.progress.messages, 1, "{:?}", second.issues);
    assert_eq!(fixture.message_count(), 4);

    // Re-reading an unchanged file imports nothing.
    let third = fixture.scan(&source);
    assert_eq!(third.progress.messages, 0);
    assert_eq!(third.progress.files_skipped, 1);
    assert_eq!(fixture.message_count(), 4);
}

#[test]
fn a_bad_line_does_not_lose_the_rest_of_the_file() {
    let fixture = Fixture::new("bad-line");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000003.jsonl",
    );
    let mut lines = codex_lines(&fixture, "019a0000-0000-7000-8000-000000000003", false);
    lines.insert(2, "{\"this is\": not json".to_string());
    write_lines(&path, &lines);
    let source = fixture.add_source("codex");
    let report = fixture.scan(&source);
    assert_eq!(report.progress.files_failed, 0);
    assert_eq!(report.progress.messages, 3);
    assert_eq!(fixture.message_count(), 3);
    let conn = fixture.service.store().connection().lock();
    let (errors, detail, offset): (i64, String, i64) = conn
        .query_row(
            "SELECT error_count, last_error_json, committed_offset FROM history_cursors",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(errors, 1);
    assert!(detail.contains("invalid-json"));
    assert!(
        !detail.contains("this is"),
        "error text must not quote content"
    );
    assert!(offset > 0);
}

#[test]
fn an_oversized_line_is_imported_truncated_with_a_flag() {
    let fixture = Fixture::new("huge-line");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000004.jsonl",
    );
    let session = "019a0000-0000-7000-8000-000000000004";
    let mut lines = codex_lines(&fixture, session, false);
    let huge = "大".repeat((model::MAX_LINE_BYTES / 3) + 1024);
    lines.push(
        json!({
            "ordinal": 4,
            "timestamp": "2026-11-01T10:00:04.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "id": "msg-huge",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": huge }]
            }
        })
        .to_string(),
    );
    write_lines(&path, &lines);
    let source = fixture.add_source("codex");
    let report = fixture.scan(&source);
    assert_eq!(report.progress.files_failed, 0, "{:?}", report.issues);
    assert_eq!(report.progress.messages, 4);
    let conn = fixture.service.store().connection().lock();
    let marker: String = conn
        .query_row(
            "SELECT content_json FROM messages WHERE content_json LIKE '%oversized-line%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(marker.contains("\"truncated\":true"));
    let errors: i64 = conn
        .query_row("SELECT error_count FROM history_cursors", [], |row| row.get(0))
        .unwrap();
    assert_eq!(errors, 1, "the oversized line is reported, not silent");
    let cursor: (i64, i64) = conn
        .query_row(
            "SELECT committed_offset, size_bytes FROM history_cursors",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        cursor.0, cursor.1,
        "the capped line is complete and advanced"
    );
}

#[test]
fn truncation_and_same_size_rewrite_replace_derived_rows() {
    let fixture = Fixture::new("rewrite");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000005.jsonl",
    );
    let session = "019a0000-0000-7000-8000-000000000005";
    let mut lines = codex_lines(&fixture, session, false);
    write_lines(&path, &lines);
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 3);

    // Same shape, same byte length, different content: only the content hash
    // can tell. The replacement text has exactly as many characters as the
    // original ("请检查历史记录与中文搜索").
    let before_len = lines[1].len();
    lines[1] = json!({
        "ordinal": 1,
        "timestamp": "2026-11-01T10:00:01.000Z",
        "type": "response_item",
        "payload": {
            "type": "message", "id": "msg-user-1", "role": "user",
            "content": [{ "type": "input_text", "text": "改写后的完全不同的正文啊" }]
        }
    })
    .to_string();
    assert_eq!(
        lines[1].len(),
        before_len,
        "the rewrite must keep the same byte length"
    );
    write_lines(&path, &lines);
    fixture.scan(&source);
    let conn = fixture.service.store().connection().lock();
    let texts: Vec<String> = {
        let mut statement = conn
            .prepare("SELECT content_json FROM messages WHERE kind = 'user-message'")
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(texts.len(), 1, "the old row must not linger");
    assert!(texts[0].contains("改写后"));
    drop(conn);

    // Truncation to an empty file removes the derived rows.
    fs::write(&path, "").unwrap();
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 0);
}

#[test]
fn a_moved_file_keeps_one_session_and_does_not_duplicate_messages() {
    let fixture = Fixture::new("move");
    let name = "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000006.jsonl";
    let original = fixture.history.join(name);
    write_lines(
        &original,
        &codex_lines(&fixture, "019a0000-0000-7000-8000-000000000006", false),
    );
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 3);
    let moved = fixture.history.join("moved").join(name);
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::rename(&original, &moved).unwrap();
    fixture.scan(&source);
    assert_eq!(
        fixture.message_count(),
        3,
        "a move must not duplicate rows"
    );
    assert_eq!(fixture.session_count(), 1);
    let conn = fixture.service.store().connection().lock();
    let path: String = conn
        .query_row("SELECT canonical_path FROM history_cursors", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(path.contains("moved"), "{path}");
}

#[test]
fn a_rotated_transcript_keeps_its_rows_and_dedupes_overlap() {
    let fixture = Fixture::new("rotate");
    let name = "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000000b.jsonl";
    let live = fixture.history.join(name);
    let session = "019a0000-0000-7000-8000-00000000000b";
    write_lines(&live, &codex_lines(&fixture, session, false));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 3);
    assert_eq!(fixture.session_count(), 1);

    // Rotate the live file aside and start a new chunk of the same native
    // session at the live path. Both files must be indexed: the archived chunk
    // keeps its rows instead of being purged by the live file's full re-read.
    let archived = fixture.history.join(format!("{name}.1"));
    fs::rename(&live, &archived).unwrap();
    let meta = json!({
        "ordinal": 0,
        "timestamp": "2026-11-01T11:00:00.000Z",
        "type": "session_meta",
        "payload": { "session_id": session, "id": session, "cwd": fixture.workspace_path() }
    });
    let added = json!({
        "ordinal": 1,
        "timestamp": "2026-11-01T11:00:01.000Z",
        "type": "response_item",
        "payload": {
            "type": "message", "id": "msg-user-2", "role": "user",
            "content": [{ "type": "input_text", "text": "轮转之后的追加消息" }]
        }
    });
    write_lines(&live, &[meta.to_string(), added.to_string()]);
    fixture.scan(&source);
    assert_eq!(
        fixture.message_count(),
        4,
        "the archived chunk must stay indexed and the new chunk added once"
    );
    assert_eq!(fixture.session_count(), 1, "same native id stays one session");

    // A second pass over both files is idempotent.
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 4);
    let conn = fixture.service.store().connection().lock();
    let archived_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM history_cursors WHERE canonical_path LIKE '%.jsonl.1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(archived_rows, 1, "the rotated chunk has its own cursor");
}

#[test]
fn compressed_codex_rollouts_are_read_and_replaced_wholesale() {
    let fixture = Fixture::new("zstd");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000007.jsonl.zst",
    );
    let lines = codex_lines(&fixture, "019a0000-0000-7000-8000-000000000007", false);
    write_zstd(&path, &(lines.join("\n") + "\n"));
    let source = fixture.add_source("codex");
    let first = fixture.scan(&source);
    assert_eq!(first.progress.files_failed, 0, "{:?}", first.issues);
    assert_eq!(first.progress.messages, 3);
    let second = fixture.scan(&source);
    assert_eq!(second.progress.files_skipped, 1);
    assert_eq!(fixture.message_count(), 3);

    let mut replaced = lines.clone();
    replaced[3] = json!({
        "ordinal": 3,
        "timestamp": "2026-11-01T10:00:03.000Z",
        "type": "response_item",
        "payload": { "type": "message", "id": "replacement", "role": "assistant",
                     "content": [{ "type": "output_text", "text": "压缩文件已替换" }] }
    })
    .to_string();
    write_zstd(&path, &(replaced.join("\n") + "\n"));
    fixture.scan(&source);
    let conn = fixture.service.store().connection().lock();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE native_event_id = 'fc-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "old compressed content must be purged");
    let replacement: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE native_event_id = 'replacement'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(replacement, 1);
}

fn write_zstd(path: &std::path::Path, text: &str) {
    use std::io::Write;
    let file = fs::File::create(path).unwrap();
    let mut encoder = zstd::stream::write::Encoder::new(file, 3).unwrap();
    encoder.write_all(text.as_bytes()).unwrap();
    encoder.finish().unwrap();
}

#[test]
fn a_failed_cursor_transaction_leaves_no_rows_behind() {
    let fixture = Fixture::new("rollback");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000008.jsonl",
    );
    write_lines(
        &path,
        &codex_lines(&fixture, "019a0000-0000-7000-8000-000000000008", false),
    );
    let source = fixture.add_source("codex");
    let conn = fixture.service.store().connection().lock();
    // Break the cursor table so the write inside the file transaction fails.
    conn.execute_batch("ALTER TABLE history_cursors RENAME TO history_cursors_blocked")
        .unwrap();
    drop(conn);
    let failed = scan_source(
        ScanInput {
            service: &fixture.service,
            source: &source,
            options: ScanOptions::incremental(),
            cancel: &|| false,
        },
        |_| {},
    );
    assert!(failed.is_err());
    let conn = fixture.service.store().connection().lock();
    let messages: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        messages, 0,
        "the message rows must roll back with the cursor"
    );
    let origins: i64 = conn
        .query_row("SELECT COUNT(*) FROM history_message_origins", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(origins, 0);
    conn.execute_batch("ALTER TABLE history_cursors_blocked RENAME TO history_cursors")
        .unwrap();
}

#[test]
fn cancellation_is_reported_and_the_next_pass_continues() {
    let fixture = Fixture::new("cancel");
    for index in 0..4 {
        let name = format!(
            "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000010{index}.jsonl"
        );
        write_lines(
            &fixture.history.join(name),
            &codex_lines(
                &fixture,
                &format!("019a0000-0000-7000-8000-00000000010{index}"),
                false,
            ),
        );
    }
    let source = fixture.add_source("codex");
    // The cancel probe must be non-blocking and must not re-enter the store:
    // the scanner may hold the only connection while it asks. The production
    // probe is a pair of atomics for the same reason.
    let done = std::sync::atomic::AtomicU64::new(0);
    let cancel_after_first = || done.load(std::sync::atomic::Ordering::SeqCst) >= 1;
    let report = scan_source(
        ScanInput {
            service: &fixture.service,
            source: &source,
            options: ScanOptions::incremental(),
            cancel: &cancel_after_first,
        },
        |progress| {
            done.store(progress.files_done, std::sync::atomic::Ordering::SeqCst);
        },
    )
    .unwrap();
    assert!(report.cancelled);
    assert!(report.progress.files_done >= 1);
    assert!(report.progress.files_done < 4);
    let resumed = fixture.scan(&source);
    assert_eq!(resumed.progress.messages, 9);
    assert_eq!(fixture.message_count(), 12);
}

// ---------------------------------------------------------------------------
// Sources, rebuild, provenance
// ---------------------------------------------------------------------------

#[test]
fn sources_are_validated_and_never_repointed() {
    let fixture = Fixture::new("source-update");
    let engine = HistoryEngine::new(
        "local-windows",
        Arc::new(MultiCliService::open(fixture.root.path.join("data")).unwrap()),
    );
    let created = engine
        .update_source(&HistorySourceUpdate {
            source_id: None,
            product: "codex".into(),
            installation_id: None,
            config_profile_id: "unconfirmed-local".into(),
            root: fixture.history.to_string_lossy().to_string(),
            enabled: true,
        })
        .unwrap();
    assert!(created.enabled);
    assert_eq!(created.status.status, "idle");
    let again = engine
        .update_source(&HistorySourceUpdate {
            source_id: Some(created.source_id.clone()),
            product: "codex".into(),
            installation_id: None,
            config_profile_id: "unconfirmed-local".into(),
            root: fixture.history.to_string_lossy().to_string(),
            enabled: false,
        })
        .unwrap();
    assert_eq!(again.source_id, created.source_id);
    assert!(!again.enabled);

    let other = fixture.root.path.join("other-root");
    fs::create_dir_all(&other).unwrap();
    let conflict = engine
        .update_source(&HistorySourceUpdate {
            source_id: Some(created.source_id.clone()),
            product: "codex".into(),
            installation_id: None,
            config_profile_id: "unconfirmed-local".into(),
            root: other.to_string_lossy().to_string(),
            enabled: true,
        })
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::Conflict);

    let bad = engine
        .update_source(&HistorySourceUpdate {
            source_id: None,
            product: "codex".into(),
            installation_id: None,
            config_profile_id: "unconfirmed-local".into(),
            root: "C:\\Windows".into(),
            enabled: true,
        })
        .unwrap_err();
    assert_eq!(bad.code, ErrorCode::InvalidArgument);
}

#[test]
fn rebuild_keeps_session_ids_notes_tags_and_removes_duplicates() {
    let fixture = Fixture::new("rebuild");
    write_lines(
        &fixture.history.join(
            "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000009.jsonl",
        ),
        &codex_lines(&fixture, "019a0000-0000-7000-8000-000000000009", false),
    );
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    let conn = fixture.service.store().connection().lock();
    let session_id: String = conn
        .query_row("SELECT session_id FROM messages LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    conn.execute(
        "INSERT INTO user_session_notes(session_id, note, updated_at_ms) VALUES (?1, ?2, ?3)",
        params![session_id, "keep this note", now_ms()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO user_session_tags(session_id, tag, created_at_ms) VALUES (?1, ?2, ?3)",
        params![session_id, "keep-tag", now_ms()],
    )
    .unwrap();
    drop(conn);

    let engine = HistoryEngine::new(
        "local-windows",
        Arc::new(MultiCliService::open(fixture.root.path.join("data")).unwrap()),
    );
    let status = engine
        .rebuild(&HistoryRebuildQuery {
            source_id: source.id.clone(),
        })
        .unwrap();
    assert_eq!(status.sources.len(), 1);
    engine.run_source_for_test(&source.id, true);

    let conn = fixture.service.store().connection().lock();
    let note: String = conn
        .query_row(
            "SELECT note FROM user_session_notes WHERE session_id = ?1",
            [session_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(note, "keep this note");
    let tag: String = conn
        .query_row(
            "SELECT tag FROM user_session_tags WHERE session_id = ?1",
            [session_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tag, "keep-tag");
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sessions, 1, "a rebuild must not create a second session");
    let messages: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(messages, 3);
    let duplicated: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (SELECT id FROM messages GROUP BY id HAVING COUNT(*) > 1)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(duplicated, 0);
}

#[test]
fn rebuilding_one_source_does_not_touch_another_sources_rows() {
    let fixture = Fixture::new("rebuild-isolation");
    let codex = fixture.add_source("codex");
    write_lines(
        &fixture.history.join(
            "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000011.jsonl",
        ),
        &codex_lines(&fixture, "019a0000-0000-7000-8000-000000000011", false),
    );
    fixture.scan(&codex);

    let second_root = fixture.root.path.join("history-root-2");
    fs::create_dir_all(&second_root).unwrap();
    let claude_product = model::product_for_agent("claude").unwrap();
    fixture
        .service
        .register_agent(&AgentDefinition {
            id: "claude".into(),
            vendor: "Anthropic".into(),
            display_name: "Claude Code".into(),
        })
        .unwrap();
    let second = {
        let conn = fixture.service.store().connection().lock();
        let source = repo::upsert_source(
            &conn,
            "local-windows",
            claude_product,
            None,
            "unconfirmed-local",
            &second_root.to_string_lossy(),
            &crate::multicli::path::canonical_text(&fs::canonicalize(&second_root).unwrap()),
            true,
            None,
        )
        .unwrap();
        repo::ensure_state(&conn, &source.id).unwrap();
        source
    };
    write_lines(
        &second_root.join("claude.jsonl"),
        &[json!({
            "type": "user",
            "uuid": "u-x",
            "sessionId": "claude-native-2",
            "cwd": fixture.workspace_path(),
            "timestamp": "2026-11-01T12:00:00.000Z",
            "message": { "role": "user", "content": "第二个来源" }
        })
        .to_string()],
    );
    fixture.scan(&second);
    let before = fixture.message_count();
    let engine = HistoryEngine::new(
        "local-windows",
        Arc::new(MultiCliService::open(fixture.root.path.join("data")).unwrap()),
    );
    engine
        .rebuild(&HistoryRebuildQuery {
            source_id: codex.id.clone(),
        })
        .unwrap();
    engine.run_source_for_test(&codex.id, true);
    let after = fixture.message_count();
    assert_eq!(
        before, after,
        "codex rows are re-imported, claude rows untouched"
    );
    let conn = fixture.service.store().connection().lock();
    let claude_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE source_key LIKE 'claude:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(claude_rows, 1);
}

// ---------------------------------------------------------------------------
// Search (A08)
// ---------------------------------------------------------------------------

fn seed_search(fixture: &Fixture) -> SourceRow {
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000012.jsonl",
    );
    let session = "019a0000-0000-7000-8000-000000000012";
    let lines = vec![
        json!({
            "ordinal": 0, "timestamp": "2026-11-01T10:00:00.000Z", "type": "session_meta",
            "payload": { "session_id": session, "id": session, "cwd": fixture.workspace_path(),
                         "timestamp": "2026-11-01T10:00:00.000Z" }
        })
        .to_string(),
        json!({
            "ordinal": 1, "timestamp": "2026-11-01T10:00:01.000Z", "type": "response_item",
            "payload": { "type": "message", "id": "s-1", "role": "user",
                "content": [{ "type": "input_text", "text": "历史记录里的中文搜索" }] }
        })
        .to_string(),
        json!({
            "ordinal": 2, "timestamp": "2026-11-01T10:00:02.000Z", "type": "response_item",
            "payload": { "type": "message", "id": "s-2", "role": "assistant",
                "content": [{ "type": "output_text", "text": "The History Recorder keeps 100% of files" }] }
        })
        .to_string(),
        json!({
            "ordinal": 3, "timestamp": "2026-11-01T10:00:03.000Z", "type": "response_item",
            "payload": { "type": "message", "id": "s-3", "role": "assistant",
                "content": [{ "type": "output_text", "text": "符号 * ? \" 引号 与 a_b 下划线" }] }
        })
        .to_string(),
        json!({
            "ordinal": 4, "timestamp": "2026-11-01T10:00:04.000Z", "type": "response_item",
            "payload": { "type": "message", "id": "s-4", "role": "assistant",
                "content": [{ "type": "output_text", "text": "表情 emoji 🎲 与短词 中文" }] }
        })
        .to_string(),
    ];
    write_lines(&path, &lines);
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    source
}

fn search(
    fixture: &Fixture,
    text: &str,
    limit: u32,
    cursor: Option<String>,
) -> super::model::SearchPage {
    query::search_history(
        &fixture.service,
        &HistorySearchQuery {
            text: text.to_string(),
            workspace_id: None,
            agent_id: None,
            source_id: None,
            limit,
            cursor,
        },
    )
    .unwrap()
}

#[test]
fn search_covers_chinese_english_short_words_and_literal_operators() {
    let fixture = Fixture::new("search");
    seed_search(&fixture);

    let chinese_long = search(&fixture, "历史记录", 20, None);
    assert_eq!(chinese_long.match_mode, "fts");
    assert_eq!(chinese_long.total, 1);
    assert!(chinese_long.items[0].snippet.contains("历史记录"));

    let chinese_short = search(&fixture, "中文", 20, None);
    assert_eq!(chinese_short.match_mode, "like");
    assert_eq!(chinese_short.total, 2, "two messages contain 中文");

    let single = search(&fixture, "史", 20, None);
    assert_eq!(single.match_mode, "like");
    assert_eq!(single.total, 1);

    let english = search(&fixture, "recorder", 20, None);
    assert_eq!(english.match_mode, "fts");
    assert_eq!(english.total, 1, "ascii search is case-insensitive");

    let emoji = search(&fixture, "🎲", 20, None);
    assert_eq!(emoji.match_mode, "like");
    assert_eq!(emoji.total, 1);

    // Every FTS operator must be literal, not syntax.
    for literal in ["*", "\"", "a_b", "100%", "(", "NEAR"] {
        let page = search(&fixture, literal, 20, None);
        assert!(page.total <= 5, "{literal} produced {page:?}");
    }
    let percent = search(&fixture, "100%", 20, None);
    assert_eq!(percent.total, 1);
    // `a_b` is three characters, so it is an FTS phrase; the wildcard escape
    // itself only matters on the 1-2 character LIKE path.
    let underscore = search(&fixture, "a_b", 20, None);
    assert_eq!(underscore.total, 1);
    let literal_percent = search(&fixture, "%", 20, None);
    assert_eq!(literal_percent.match_mode, "like");
    assert_eq!(
        literal_percent.total, 1,
        "a literal % must not become a wildcard"
    );
    let literal_underscore = search(&fixture, "_", 20, None);
    assert_eq!(literal_underscore.match_mode, "like");
    assert_eq!(
        literal_underscore.total, 1,
        "a literal _ must not become a wildcard"
    );
    assert_eq!(
        query::search_history(
            &fixture.service,
            &HistorySearchQuery {
                text: "   ".into(),
                workspace_id: None,
                agent_id: None,
                source_id: None,
                limit: 20,
                cursor: None,
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn search_pages_stably_and_filters_by_workspace_and_source() {
    let fixture = Fixture::new("search-page");
    let source = seed_search(&fixture);
    let first = search(&fixture, "中文", 1, None);
    assert_eq!(first.total, 2);
    assert_eq!(first.items.len(), 1);
    let cursor = first.next_cursor.clone().expect("second page");
    let second = search(&fixture, "中文", 1, Some(cursor));
    assert_eq!(second.total, 2);
    assert_eq!(second.items.len(), 1);
    assert_ne!(first.items[0].message_id, second.items[0].message_id);

    let conn = fixture.service.store().connection().lock();
    let workspace_id: String = conn
        .query_row(
            "SELECT workspace_id FROM session_workspaces WHERE role = 'primary'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);
    let by_workspace = query::search_history(
        &fixture.service,
        &HistorySearchQuery {
            text: "中文".into(),
            workspace_id: Some(workspace_id),
            agent_id: None,
            source_id: None,
            limit: 20,
            cursor: None,
        },
    )
    .unwrap();
    assert_eq!(by_workspace.total, 2);
    let by_source = query::search_history(
        &fixture.service,
        &HistorySearchQuery {
            text: "中文".into(),
            workspace_id: None,
            agent_id: None,
            source_id: Some(source.id.clone()),
            limit: 20,
            cursor: None,
        },
    )
    .unwrap();
    assert_eq!(by_source.total, 2);
    let by_other_source = query::search_history(
        &fixture.service,
        &HistorySearchQuery {
            text: "中文".into(),
            workspace_id: None,
            agent_id: None,
            source_id: Some("hsrc-missing".into()),
            limit: 20,
            cursor: None,
        },
    )
    .unwrap();
    assert_eq!(by_other_source.total, 0);
    let by_agent = query::search_history(
        &fixture.service,
        &HistorySearchQuery {
            text: "中文".into(),
            workspace_id: None,
            agent_id: Some("claude".into()),
            source_id: None,
            limit: 20,
            cursor: None,
        },
    )
    .unwrap();
    assert_eq!(by_agent.total, 0);
}

#[test]
fn search_results_point_at_the_same_session_and_message() {
    let fixture = Fixture::new("search-link");
    seed_search(&fixture);
    let page = search(&fixture, "历史记录", 20, None);
    let hit = &page.items[0];
    let metadata = query::session_metadata(&fixture.service, &hit.session_id).unwrap();
    assert_eq!(metadata.message_count, 4);
    let messages = query::message_page(
        &fixture.service,
        &MessagePageQuery {
            session_id: hit.session_id.clone(),
            limit: 20,
            cursor: None,
            anchor_message_id: Some(hit.message_id.clone()),
        },
    )
    .unwrap();
    assert!(messages
        .items
        .iter()
        .any(|message| message.id == hit.message_id));
    assert!(messages.anchor_index.is_some());
    assert_eq!(fixture.session_count(), 1);
}

// ---------------------------------------------------------------------------
// Bounded reads
// ---------------------------------------------------------------------------

#[test]
fn message_pages_are_bounded_and_anchored() {
    let fixture = Fixture::new("message-page");
    seed_search(&fixture);
    let session_id = {
        let conn = fixture.service.store().connection().lock();
        conn.query_row("SELECT id FROM sessions LIMIT 1", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap()
    };
    let first = query::message_page(
        &fixture.service,
        &MessagePageQuery {
            session_id: session_id.clone(),
            limit: 2,
            cursor: None,
            anchor_message_id: None,
        },
    )
    .unwrap();
    assert_eq!(first.items.len(), 2);
    assert_eq!(first.total, 4);
    assert!(first.next_cursor.is_some());
    let second = query::message_page(
        &fixture.service,
        &MessagePageQuery {
            session_id: session_id.clone(),
            limit: 10,
            cursor: first.next_cursor.clone(),
            anchor_message_id: None,
        },
    )
    .unwrap();
    assert_eq!(second.items.len(), 2);
    assert!(second.next_cursor.is_none());
    assert_ne!(first.items[0].id, second.items[0].id);

    let anchor_id = first.items[1].id.clone();
    let anchored = query::message_page(
        &fixture.service,
        &MessagePageQuery {
            session_id: session_id.clone(),
            limit: 2,
            cursor: None,
            anchor_message_id: Some(anchor_id.clone()),
        },
    )
    .unwrap();
    assert_eq!(anchored.items.len(), 2);
    assert_eq!(
        anchored.anchor_message_id.as_deref(),
        Some(anchor_id.as_str())
    );
    let index = anchored.anchor_index.unwrap() as usize;
    assert_eq!(anchored.items[index].id, anchor_id);
    assert!(anchored.next_cursor.is_some());

    let missing = query::message_page(
        &fixture.service,
        &MessagePageQuery {
            session_id: session_id.clone(),
            limit: 2,
            cursor: None,
            anchor_message_id: Some("hmsg-missing".into()),
        },
    )
    .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
    let not_found = query::message_page(
        &fixture.service,
        &MessagePageQuery {
            session_id: "hist-missing".into(),
            limit: 2,
            cursor: None,
            anchor_message_id: None,
        },
    )
    .unwrap_err();
    assert_eq!(not_found.code, ErrorCode::NotFound);
}

#[test]
fn session_metadata_does_not_embed_messages_and_reports_provenance() {
    let fixture = Fixture::new("metadata");
    seed_search(&fixture);
    let conn = fixture.service.store().connection().lock();
    let session_id: String = conn
        .query_row("SELECT id FROM sessions LIMIT 1", [], |row| row.get(0))
        .unwrap();
    drop(conn);
    let metadata = query::session_metadata(&fixture.service, &session_id).unwrap();
    let wire = serde_json::to_value(&metadata).unwrap();
    assert!(
        wire.get("messages").is_none(),
        "metadata must not embed messages"
    );
    assert_eq!(metadata.message_count, 4);
    assert_eq!(metadata.run_total, 0);
    assert!(metadata.runs.is_empty());
    assert_eq!(metadata.history.as_ref().unwrap().file_count, 1);
    assert_eq!(metadata.history.as_ref().unwrap().parser_versions, vec![1]);
    assert!(!metadata.message_kinds.is_empty());
}

// ---------------------------------------------------------------------------
// Resume target (A14)
// ---------------------------------------------------------------------------

#[test]
fn resume_resolution_is_read_only_and_reports_conflicts() {
    let fixture = Fixture::new("resume");
    seed_search(&fixture);
    let conn = fixture.service.store().connection().lock();
    let session_id: String = conn
        .query_row("SELECT id FROM sessions LIMIT 1", [], |row| row.get(0))
        .unwrap();
    drop(conn);

    // No installation yet: unknown, not resolved.
    let unknown = resume::resolve_resume_target(&fixture.service, &session_id).unwrap();
    assert_eq!(unknown.status, "unknown");
    assert!(!unknown.execution_available);
    assert!(unknown.conflicts.is_empty());

    // A confirmed installation plus no live process resolves the target.
    fixture
        .service
        .register_installation(&AgentInstallation {
            id: "install-codex".into(),
            host_id: "local-windows".into(),
            agent_id: "codex".into(),
            executable: HostPath {
                host_id: "local-windows".into(),
                display_path: "C:\\Tools\\codex.exe".into(),
                canonical_path: Some("C:\\Tools\\codex.exe".into()),
            },
            version: Some("0.166.0".into()),
            config_profile_id: "unconfirmed-local".into(),
        })
        .unwrap();
    let resolved = resume::resolve_resume_target(&fixture.service, &session_id).unwrap();
    assert_eq!(resolved.status, "resolved");
    assert_eq!(
        resolved.native_id.as_deref(),
        Some("019a0000-0000-7000-8000-000000000012")
    );
    assert_eq!(resolved.product.as_deref(), Some("codex"));
    assert!(resolved.installation_id.is_some());
    assert!(!resolved.execution_available);

    // An active run blocks the target.
    fixture
        .service
        .start_run(&Run {
            id: "run-active".into(),
            session_id: session_id.clone(),
            installation_id: Some("install-codex".into()),
            transport: RunTransport::Native,
            ownership: RunOwnership::Managed,
            started_at: Some("2026-11-01T10:00:00.000Z".into()),
            ended_at: None,
            execution_status: ExecutionStatus::Running,
            interaction_status: InteractionStatus::Working,
            evidence_level: crate::multicli::domain::EvidenceLevel::Observed,
            evidence: Some("synthetic".into()),
        })
        .unwrap();
    let blocked = resume::resolve_resume_target(&fixture.service, &session_id).unwrap();
    assert_eq!(blocked.status, "blocked");
    assert_eq!(blocked.conflicts[0].kind, "active-run");
    let runs_before = {
        let conn = fixture.service.store().connection().lock();
        conn.query_row("SELECT COUNT(*) FROM runs", [], |row| row.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(runs_before, 1, "resolution must not create a run");
}

#[test]
fn resume_is_unsupported_without_a_native_id() {
    let fixture = Fixture::new("resume-unsupported");
    fixture
        .service
        .register_agent(&AgentDefinition {
            id: "codex".into(),
            vendor: "OpenAI".into(),
            display_name: "Codex CLI".into(),
        })
        .unwrap();
    let session = Session {
        id: "hist-manual".into(),
        agent_id: "codex".into(),
        host_id: "local-windows".into(),
        source_namespace: "history:codex:codex:manual".into(),
        native_id: None,
        origin: SessionOrigin::HistoryOnly,
        title: "no native id".into(),
    };
    fixture.service.ensure_session(&session).unwrap();
    let target = resume::resolve_resume_target(&fixture.service, "hist-manual").unwrap();
    assert_eq!(target.status, "unsupported");
    assert_eq!(target.conflicts[0].kind, "missing-native-id");
    assert!(!target.execution_available);
}

// ---------------------------------------------------------------------------
// Source isolation, hashes, schema v4
// ---------------------------------------------------------------------------

#[test]
fn import_never_modifies_the_source_file() {
    let fixture = Fixture::new("hash");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000013.jsonl",
    );
    write_lines(
        &path,
        &codex_lines(&fixture, "019a0000-0000-7000-8000-000000000013", false),
    );
    let before = ids::sha256_hex(&fs::read(&path).unwrap());
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    fixture.scan(&source);
    let after = ids::sha256_hex(&fs::read(&path).unwrap());
    assert_eq!(before, after);
    let conn = fixture.service.store().connection().lock();
    let cursor: (String, String) = conn
        .query_row(
            "SELECT head_hash, file_identity FROM history_cursors",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(cursor.0.len(), 64);
    assert!(cursor.1.starts_with("path:"), "{}", cursor.1);
}

#[test]
fn schema_v4_adds_history_tables_without_touching_s03_data() {
    let root = TempRoot::new("v4-migration");
    {
        let service = MultiCliService::open_for_test(
            &root.path,
            MigrationFault {
                stop_after_version: Some(3),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(service.schema_version(), 3);
        service
            .register_host(&Host::local("local-windows"))
            .unwrap();
        service
            .register_agent(&AgentDefinition {
                id: "codex".into(),
                vendor: "OpenAI".into(),
                display_name: "Codex CLI".into(),
            })
            .unwrap();
        service
            .register_workspace(&Workspace {
                id: "ws-keep".into(),
                path: HostPath {
                    host_id: "local-windows".into(),
                    display_path: "C:\\Keep".into(),
                    canonical_path: Some("C:\\Keep".into()),
                },
                repo_group_id: None,
                worktree_id: None,
            })
            .unwrap();
        service
            .ensure_session(&Session {
                id: "s-keep".into(),
                agent_id: "codex".into(),
                host_id: "local-windows".into(),
                source_namespace: "manual".into(),
                native_id: Some("native-keep".into()),
                origin: SessionOrigin::Managed,
                title: "keep me".into(),
            })
            .unwrap();
        service
            .execute_raw(
                "INSERT INTO user_session_notes(session_id, note, updated_at_ms)
                 VALUES ('s-keep', 'note across v4', 1700000000000)",
            )
            .unwrap();
    }
    {
        let conn = Connection::open(root.path.join("multicli.db")).unwrap();
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, 3);
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'history_sources'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }

    // A failure inside v4 must roll back to v3 with no history table left.
    let error = match MultiCliService::open_for_test(
        &root.path,
        MigrationFault {
            fail_at: Some((4, 1)),
            ..Default::default()
        },
    ) {
        Ok(_) => panic!("injected v4 migration failure did not fail"),
        Err(error) => error,
    };
    assert_eq!(error.details["failedVersion"], 4);
    {
        let conn = Connection::open(root.path.join("multicli.db")).unwrap();
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, 3);
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'history_sources'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0, "partial v4 DDL must roll back");
        let notes: i64 = conn
            .query_row("SELECT COUNT(*) FROM user_session_notes", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(notes, 1);
    }

    let service = MultiCliService::open(&root.path).unwrap();
    assert_eq!(service.schema_version(), SCHEMA_VERSION);
    let conn = service.store().connection().lock();
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let note: String = conn
        .query_row(
            "SELECT note FROM user_session_notes WHERE session_id = 's-keep'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(note, "note across v4");
    let fts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'history_message_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fts, 1);
    let violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn disabled_sources_are_not_refreshed_implicitly() {
    let fixture = Fixture::new("disabled");
    write_lines(
        &fixture.history.join(
            "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000014.jsonl",
        ),
        &codex_lines(&fixture, "019a0000-0000-7000-8000-000000000014", false),
    );
    let source = fixture.add_source("codex");
    {
        let conn = fixture.service.store().connection().lock();
        repo::set_enabled(&conn, &source.id, false).unwrap();
    }
    let engine = HistoryEngine::new(
        "local-windows",
        Arc::new(MultiCliService::open(fixture.root.path.join("data")).unwrap()),
    );
    let error = engine
        .refresh(&super::model::HistoryRefreshQuery {
            source_id: Some(source.id.clone()),
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(fixture.message_count(), 0);
}

#[test]
fn cursor_rows_survive_reopen() {
    let fixture = Fixture::new("cursor-reopen");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000015.jsonl",
    );
    write_lines(
        &path,
        &codex_lines(&fixture, "019a0000-0000-7000-8000-000000000015", false),
    );
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    let before: (i64, String) = {
        let conn = fixture.service.store().connection().lock();
        conn.query_row(
            "SELECT committed_offset, file_identity FROM history_cursors",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };
    let reopened = MultiCliService::open(fixture.root.path.join("data")).unwrap();
    let conn = reopened.store().connection().lock();
    let after: (i64, String) = conn
        .query_row(
            "SELECT committed_offset, file_identity FROM history_cursors",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(before, after);
    let cursor: Option<CursorRow> = repo::read_cursor(&conn, &source.id, &after.1).unwrap();
    assert!(cursor.is_some());
}

// ---------------------------------------------------------------------------
// Audit follow-ups: restart, unavailable root, three entries, identity
// ---------------------------------------------------------------------------

#[test]
fn a_restart_preserves_message_text_and_order() {
    let mut fixture = Fixture::new("restart-body");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000016.jsonl",
    );
    write_lines(
        &path,
        &codex_lines(&fixture, "019a0000-0000-7000-8000-000000000016", false),
    );
    let source = fixture.add_source("codex");
    fixture.scan(&source);

    let snapshot = |service: &MultiCliService| -> Vec<(String, i64, String, String)> {
        let conn = service.store().connection().lock();
        let mut statement = conn
            .prepare(
                "SELECT id, seq, observed_at, content_json FROM messages
                 ORDER BY observed_at, seq, id",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let before = snapshot(&fixture.service);
    assert_eq!(before.len(), 3);

    fixture.restart();
    let after = snapshot(&fixture.service);
    assert_eq!(before, after, "restart must not reorder or rewrite rows");

    // The index is still usable after the restart: a re-scan skips the file and
    // a search still finds the same message.
    let report = fixture.scan(&source);
    assert_eq!(report.progress.messages, 0);
    assert_eq!(report.progress.files_skipped, 1);
    let page = query::search_history(
        &fixture.service,
        &HistorySearchQuery {
            text: "历史记录".into(),
            workspace_id: None,
            agent_id: None,
            source_id: None,
            limit: 10,
            cursor: None,
        },
    )
    .unwrap();
    assert_eq!(page.total, 1);
}

#[test]
fn an_unavailable_root_fails_the_pass_without_touching_rows() {
    let fixture = Fixture::new("root-gone");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000017.jsonl",
    );
    write_lines(
        &path,
        &codex_lines(&fixture, "019a0000-0000-7000-8000-000000000017", false),
    );
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 3);

    fs::remove_dir_all(&fixture.history).unwrap();
    let error = scan_source(
        ScanInput {
            service: &fixture.service,
            source: &source,
            options: ScanOptions::incremental(),
            cancel: &|| false,
        },
        |_| {},
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Io);
    assert!(error.retryable);
    assert_eq!(
        fixture.message_count(),
        3,
        "a missing root must not delete already imported rows"
    );
}

#[test]
fn the_same_session_is_reached_from_search_workspace_and_agent_filters() {
    let fixture = Fixture::new("three-entries");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000018.jsonl",
    );
    write_lines(
        &path,
        &codex_lines(&fixture, "019a0000-0000-7000-8000-000000000018", false),
    );
    let source = fixture.add_source("codex");
    fixture.scan(&source);

    let (session_id, workspace_id) = {
        let conn = fixture.service.store().connection().lock();
        let session_id: String = conn
            .query_row("SELECT id FROM sessions", [], |row| row.get(0))
            .unwrap();
        let workspace_id: String = conn
            .query_row(
                "SELECT id FROM workspaces WHERE canonical_path = ?1",
                [fixture.workspace_path()],
                |row| row.get(0),
            )
            .unwrap();
        (session_id, workspace_id)
    };

    let search = |workspace_id: Option<&str>, agent_id: Option<&str>| {
        query::search_history(
            &fixture.service,
            &HistorySearchQuery {
                text: "历史记录".into(),
                workspace_id: workspace_id.map(str::to_string),
                agent_id: agent_id.map(str::to_string),
                source_id: None,
                limit: 10,
                cursor: None,
            },
        )
        .unwrap()
    };
    let plain = search(None, None);
    assert_eq!(plain.total, 1);
    assert_eq!(plain.items[0].session_id, session_id);
    let by_workspace = search(Some(&workspace_id), None);
    assert_eq!(by_workspace.total, 1);
    assert_eq!(by_workspace.items[0].session_id, session_id);
    assert_eq!(by_workspace.items[0].message_id, plain.items[0].message_id);
    let by_agent = search(None, Some("codex"));
    assert_eq!(by_agent.total, 1);
    assert_eq!(by_agent.items[0].session_id, session_id);
    assert_eq!(
        by_agent.items[0].session_message_count,
        plain.items[0].session_message_count
    );

    let metadata = query::session_metadata(&fixture.service, &session_id).unwrap();
    let page = query::message_page(
        &fixture.service,
        &MessagePageQuery {
            session_id: session_id.clone(),
            limit: 50,
            cursor: None,
            anchor_message_id: None,
        },
    )
    .unwrap();
    assert_eq!(page.total, metadata.message_count);
    assert_eq!(
        page.items.first().map(|message| message.id.clone()),
        Some(by_workspace.items[0].message_id.clone()),
        "the search hit must be the first message of the same session"
    );
}

#[test]
fn a_second_source_with_a_different_path_case_is_the_same_source() {
    let fixture = Fixture::new("path-case");
    let source = fixture.add_source("codex");
    let product = model::product_for_agent("codex").unwrap();
    let upper = source.root_canonical_path.to_uppercase();
    let conn = fixture.service.store().connection().lock();
    let again = repo::upsert_source(
        &conn,
        "local-windows",
        product,
        None,
        "unconfirmed-local",
        &source.root_display_path,
        &upper,
        true,
        None,
    )
    .unwrap();
    assert_eq!(again.id, source.id, "Windows paths are case-insensitive");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM history_sources", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn history_does_not_overwrite_the_discovered_agent_identity() {
    let fixture = Fixture::new("agent-identity");
    fixture
        .service
        .register_agent(&AgentDefinition {
            id: "codex".into(),
            vendor: "S02-Discovered".into(),
            display_name: "S02 Codex".into(),
        })
        .unwrap();
    query::ensure_agent_for_product(&fixture.service, "codex").unwrap();
    let conn = fixture.service.store().connection().lock();
    let (vendor, display): (String, String) = conn
        .query_row(
            "SELECT vendor, display_name FROM agents WHERE id = 'codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(vendor, "S02-Discovered");
    assert_eq!(display, "S02 Codex");
}

#[test]
fn stale_in_flight_state_is_cleared() {
    let fixture = Fixture::new("stale-inflight");
    let source = fixture.add_source("codex");
    {
        let conn = fixture.service.store().connection().lock();
        conn.execute(
            "UPDATE history_index_state SET in_flight = 1, status = 'indexing'",
            [],
        )
        .unwrap();
    }
    let mut conn = fixture.service.store().connection().lock();
    let cleared = repo::reset_in_flight(&mut conn, "local-windows").unwrap();
    assert_eq!(cleared, 1);
    let state = repo::ensure_state(&conn, &source.id).unwrap();
    assert!(!state.in_flight);
    assert_eq!(state.status, "cancelled");
}

#[test]
fn control_and_bidi_characters_do_not_reach_tool_input_or_title() {
    let fixture = Fixture::new("control-chars");
    let path = fixture.history.join(
        "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-000000000019.jsonl",
    );
    let session = "019a0000-0000-7000-8000-000000000019";
    let meta = json!({
        "ordinal": 0,
        "timestamp": "2026-11-01T10:00:00.000Z",
        "type": "session_meta",
        "payload": { "session_id": session, "id": session, "cwd": fixture.workspace_path() }
    });
    let user = json!({
        "ordinal": 1,
        "timestamp": "2026-11-01T10:00:01.000Z",
        "type": "response_item",
        "payload": {
            "type": "message", "id": "msg-user-ctl", "role": "user",
            "content": [{ "type": "input_text", "text": "正常标题\u{202e}gnp.exe\u{1b}[2K" }]
        }
    });
    let tool = json!({
        "ordinal": 2,
        "timestamp": "2026-11-01T10:00:02.000Z",
        "type": "response_item",
        "payload": {
            "type": "function_call", "id": "fc-ctl", "name": "shell",
            "arguments": "{\"cmd\":\"dir\u{1b}[31m\u{202e}\"}", "call_id": "call-ctl"
        }
    });
    write_lines(&path, &[meta.to_string(), user.to_string(), tool.to_string()]);
    let source = fixture.add_source("codex");
    fixture.scan(&source);

    let conn = fixture.service.store().connection().lock();
    let title: String = conn
        .query_row("SELECT title FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert!(title.starts_with("正常标题"), "{title}");
    assert!(!title.contains('\u{1b}'), "escape must be stripped");
    assert!(!title.contains('\u{202e}'), "bidi override must be stripped");
    let body: String = conn
        .query_row(
            "SELECT content_json FROM messages WHERE kind = 'tool-call'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !body.contains('\u{1b}'),
        "escape must be stripped from tool input"
    );
    assert!(!body.contains('\u{202e}'), "bidi override must be stripped");
    assert!(body.contains("dir"));
}

#[test]
fn a_scan_can_start_mid_listing_and_wraps_to_the_end() {
    let fixture = Fixture::new("rotation-slices");
    let names = [
        "019a0000-0000-7000-8000-000000000021",
        "019a0000-0000-7000-8000-000000000022",
        "019a0000-0000-7000-8000-000000000023",
        "019a0000-0000-7000-8000-000000000024",
    ];
    for session in names {
        let path = fixture
            .history
            .join(format!("rollout-2026-11-01T10-00-00-{session}.jsonl"));
        write_lines(&path, &codex_lines(&fixture, session, false));
    }
    let source = fixture.add_source("codex");

    // Start at the third file. The pass covers the rest and wraps to the first
    // two instead of stopping at the end of the listing; a large root stops on
    // the budget instead and reports where to continue.
    let first = fixture.scan_with(
        &source,
        ScanOptions {
            start_index: 2,
            ..ScanOptions::incremental()
        },
    );
    assert_eq!(first.progress.files_done, 4);
    assert_eq!(first.next_index, 0, "a full cycle wraps back to zero");
    assert!(first.complete);
    assert_eq!(fixture.message_count(), 12);
    assert_eq!(fixture.session_count(), 4);

    // Another pass is idempotent and starts from the beginning again.
    let second = fixture.scan_with(
        &source,
        ScanOptions {
            start_index: first.next_index,
            ..ScanOptions::incremental()
        },
    );
    assert_eq!(second.progress.files_done, 0);
    assert_eq!(second.progress.files_skipped, 4);
    assert!(second.complete);
    assert_eq!(fixture.message_count(), 12);
}

#[test]
fn the_unfiltered_fast_path_matches_the_joined_path() {
    let fixture = Fixture::new("fast-vs-joined");
    for (index, session) in [
        "019a0000-0000-7000-8000-000000000031",
        "019a0000-0000-7000-8000-000000000032",
    ]
    .iter()
    .enumerate()
    {
        let path = fixture
            .history
            .join(format!("rollout-2026-11-01T10-00-00-{session}.jsonl"));
        write_lines(&path, &codex_lines(&fixture, session, index == 1));
    }
    let source = fixture.add_source("codex");
    fixture.scan(&source);

    let search = |agent: Option<&str>| {
        query::search_history(
            &fixture.service,
            &HistorySearchQuery {
                text: "历史记录".into(),
                workspace_id: None,
                agent_id: agent.map(str::to_string),
                source_id: None,
                limit: 50,
                cursor: None,
            },
        )
        .unwrap()
    };
    // No filter takes the FTS-only fast path; the agent filter takes the join
    // path. Both must return the same ids in the same order and the same total.
    let fast = search(None);
    let joined = search(Some("codex"));
    assert_eq!(fast.total, joined.total);
    assert_eq!(fast.match_mode, joined.match_mode);
    let fast_ids: Vec<String> = fast.items.iter().map(|hit| hit.message_id.clone()).collect();
    let joined_ids: Vec<String> = joined
        .items
        .iter()
        .map(|hit| hit.message_id.clone())
        .collect();
    assert_eq!(fast_ids, joined_ids);
    assert_eq!(fast.items.len(), 2, "one matching user message per session");
}
// ---------------------------------------------------------------------------
// S03-R1 regression tests
//
// Each block maps to one rework item. They are written against the public
// history API, not against a private helper, so they keep failing if the
// behaviour regresses for a different reason.
// ---------------------------------------------------------------------------

fn write_text(path: &std::path::Path, text: &str) {
    fs::write(path, text).unwrap();
}

fn append_text(path: &std::path::Path, text: &str) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// A Codex transcript with `count` user messages and no native event ids, so
/// the fallback key (line index) is what keeps them apart.
fn simple_codex(fixture: &Fixture, session: &str, count: usize) -> Vec<String> {
    let mut lines = vec![json!({
        "ordinal": 0,
        "timestamp": "2026-11-01T10:00:00.000Z",
        "type": "session_meta",
        "payload": {
            "session_id": session,
            "id": session,
            "cwd": fixture.workspace_path(),
            "cli_version": "0.166.0",
        }
    })
    .to_string()];
    for index in 1..=count {
        lines.push(
            json!({
                "ordinal": index,
                "timestamp": format!("2026-11-01T10:00:{:02}.000Z", index.min(59)),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": format!("消息 {index}") }]
                }
            })
            .to_string(),
        );
    }
    lines
}

fn transcript_text(lines: &[String]) -> String {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

// --- R1-01 -----------------------------------------------------------------

#[test]
fn a_byte_budget_stop_is_reported_incomplete_and_the_next_pass_finishes() {
    let fixture = Fixture::new("r1-budget-plain");
    let session = "019a0000-0000-7000-8000-00000000r101";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r101.jsonl");
    write_text(&path, &transcript_text(&simple_codex(&fixture, session, 12)));
    let source = fixture.add_source("codex");
    let options = ScanOptions {
        max_bytes_per_file: 400,
        budget_ms: 20_000,
        ..ScanOptions::incremental()
    };

    let first = fixture.scan_with(&source, options.clone());
    assert!(
        !first.complete,
        "a file that stopped on its byte budget is not a complete pass"
    );
    assert_eq!(first.progress.files_incomplete, 1);
    let after_first = fixture.message_count();
    assert!(
        after_first > 0 && after_first < 12,
        "expected a partial import, got {after_first}"
    );

    // Ten more passes must eventually reach every record, and the unchanged
    // size/mtime must not make the scanner treat it as done.
    for _ in 0..12 {
        fixture.scan_with(&source, options.clone());
    }
    assert_eq!(fixture.message_count(), 12);
    let conn = fixture.service.store().connection().lock();
    let complete: i64 = conn
        .query_row("SELECT read_complete FROM history_cursors", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(complete, 1);
}

#[test]
fn a_compressed_budget_stop_resumes_from_the_logical_checkpoint() {
    let fixture = Fixture::new("r1-budget-zstd");
    let session = "019a0000-0000-7000-8000-00000000r102";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r102.jsonl.zst");
    write_zstd(&path, &transcript_text(&simple_codex(&fixture, session, 12)));
    let source = fixture.add_source("codex");
    let options = ScanOptions {
        max_bytes_per_file: 400,
        budget_ms: 20_000,
        ..ScanOptions::incremental()
    };

    let first = fixture.scan_with(&source, options.clone());
    assert!(!first.complete);
    assert!(fixture.message_count() < 12);

    for _ in 0..12 {
        fixture.scan_with(&source, options.clone());
    }
    assert_eq!(
        fixture.message_count(),
        12,
        "a compressed stream must resume at its logical offset"
    );
}

#[test]
fn a_completed_file_reports_complete_and_stops_re_reading() {
    let fixture = Fixture::new("r1-complete");
    let session = "019a0000-0000-7000-8000-00000000r103";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r103.jsonl");
    write_text(&path, &transcript_text(&simple_codex(&fixture, session, 3)));
    let source = fixture.add_source("codex");
    let first = fixture.scan(&source);
    assert!(first.complete);
    assert_eq!(first.progress.files_incomplete, 0);
    let second = fixture.scan(&source);
    assert!(second.complete);
    assert_eq!(second.progress.messages, 0);
    assert_eq!(second.progress.files_skipped, 1);
}

// --- R1-02 -----------------------------------------------------------------

#[test]
fn an_append_without_native_event_ids_keeps_one_message_per_line() {
    let fixture = Fixture::new("r1-fallback-ids");
    let session = "019a0000-0000-7000-8000-00000000r111";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r111.jsonl");
    let mut lines = simple_codex(&fixture, session, 1);
    // Each message is larger than the 4 KiB head sample, which is what made the
    // fallback identity collide in the reported counterexample.
    lines[1] = json!({
        "ordinal": 1,
        "timestamp": "2026-11-01T10:00:01.000Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": format!("FIRST{}", "x".repeat(5000)) }]
        }
    })
    .to_string();
    write_text(&path, &transcript_text(&lines));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 1);

    for (index, label) in [(2, "SECOND"), (3, "THIRD"), (4, "FOURTH")] {
        append_text(
            &path,
            &(json!({
                "ordinal": index,
                "timestamp": format!("2026-11-01T10:00:{index:02}.000Z"),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": format!("{label}{}", "x".repeat(5000)) }]
                }
            })
            .to_string()
                + "\n"),
        );
        fixture.scan(&source);
    }
    assert_eq!(
        fixture.message_count(),
        4,
        "the line counter must continue across passes"
    );
    let conn = fixture.service.store().connection().lock();
    let distinct: i64 = conn
        .query_row("SELECT COUNT(DISTINCT source_key) FROM messages", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(distinct, 4);
}

#[test]
fn a_renamed_transcript_keeps_its_header_session_on_append() {
    let fixture = Fixture::new("r1-header-identity");
    let session = "019a0000-0000-7000-8000-00000000r112";
    let path = fixture.history.join("renamed.jsonl");
    let lines = simple_codex(&fixture, session, 1);
    write_text(&path, &transcript_text(&lines));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.session_count(), 1);

    append_text(&path, &(lines[1].clone() + "\n"));
    fixture.scan(&source);
    assert_eq!(
        fixture.session_count(),
        1,
        "an append must restore the header identity instead of guessing from the file name"
    );
    assert_eq!(fixture.message_count(), 2);
}

#[test]
fn a_restart_between_appends_keeps_identity_and_line_numbers() {
    let mut fixture = Fixture::new("r1-restart-append");
    let session = "019a0000-0000-7000-8000-00000000r113";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r113.jsonl");
    let lines = simple_codex(&fixture, session, 2);
    write_text(&path, &transcript_text(&lines));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    fixture.restart();

    append_text(&path, &(lines[2].clone() + "\n"));
    fixture.scan(&source);
    assert_eq!(fixture.session_count(), 1);
    assert_eq!(fixture.message_count(), 3);
}

// --- R1-03 -----------------------------------------------------------------

#[test]
fn two_files_of_one_source_both_keep_provenance() {
    let fixture = Fixture::new("r1-provenance");
    let session = "019a0000-0000-7000-8000-00000000r121";
    let text = transcript_text(&simple_codex(&fixture, session, 2));
    let first = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r121.jsonl");
    let copy = fixture.history.join("z-copy.jsonl");
    write_text(&first, &text);
    write_text(&copy, &text);
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 2, "identical events dedupe by id");

    {
        let conn = fixture.service.store().connection().lock();
        let origins: i64 = conn
            .query_row("SELECT COUNT(*) FROM history_message_origins", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(origins, 4, "each file keeps its own contribution row");
    }

    // Truncating the duplicate must not delete what the first file still
    // provides.
    write_text(&copy, "");
    fixture.scan(&source);
    assert_eq!(
        fixture.message_count(),
        2,
        "one file's truncate removed another file's messages"
    );
    let conn = fixture.service.store().connection().lock();
    let origins: i64 = conn
        .query_row("SELECT COUNT(*) FROM history_message_origins", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(origins, 2);
}

#[test]
fn a_rebuild_removes_only_unreferenced_messages() {
    let fixture = Fixture::new("r1-rebuild-provenance");
    let session = "019a0000-0000-7000-8000-00000000r122";
    let text = transcript_text(&simple_codex(&fixture, session, 3));
    let first = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r122.jsonl");
    let copy = fixture.history.join("z-copy.jsonl");
    write_text(&first, &text);
    write_text(&copy, &text);
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    {
        let mut conn = fixture.service.store().connection().lock();
        let report = repo::purge_source_derived(&mut conn, &source.id).unwrap();
        assert_eq!(report.messages_deleted, 3);
    }
    assert_eq!(fixture.message_count(), 0);
    assert_eq!(fixture.session_count(), 1, "the stable session row stays");
}

// --- R1-04 -----------------------------------------------------------------

#[test]
fn a_nested_protected_directory_is_never_walked() {
    let fixture = Fixture::new("r1-nested-credentials");
    let session = "019a0000-0000-7000-8000-00000000r131";
    let nested = fixture.history.join(".ssh");
    fs::create_dir_all(&nested).unwrap();
    write_text(
        &nested.join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r131.jsonl"),
        &transcript_text(&simple_codex(&fixture, session, 2)),
    );
    let ok_session = "019a0000-0000-7000-8000-00000000r132";
    write_text(
        &fixture
            .history
            .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r132.jsonl"),
        &transcript_text(&simple_codex(&fixture, ok_session, 1)),
    );
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(
        fixture.message_count(),
        1,
        "only the non-protected transcript is imported"
    );
    assert_eq!(fixture.session_count(), 1);
}

#[cfg(windows)]
#[test]
fn a_junction_below_the_root_is_not_followed() {
    let fixture = Fixture::new("r1-junction");
    let outside = fixture.root.path.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let session = "019a0000-0000-7000-8000-00000000r133";
    write_text(
        &outside.join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r133.jsonl"),
        &transcript_text(&simple_codex(&fixture, session, 2)),
    );
    let link = fixture.history.join("linked");
    let status = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&link)
        .arg(&outside)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run mklink /J");
    if !status.success() {
        // A machine without junction support cannot exercise this path; the
        // name-based guard is still covered above.
        return;
    }
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 0, "a junction is never followed");
}

// --- R1-05 -----------------------------------------------------------------

#[test]
fn a_same_size_middle_rewrite_is_found_by_the_verification_cycle() {
    let fixture = Fixture::new("r1-middle-rewrite");
    let session = "019a0000-0000-7000-8000-00000000r141";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r141.jsonl");
    let mut lines = simple_codex(&fixture, session, 0);
    lines.push(
        json!({
            "ordinal": 1, "timestamp": "2026-11-01T10:00:01.000Z", "type": "response_item",
            "payload": { "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "a".repeat(6000) }] }
        })
        .to_string(),
    );
    lines.push(
        json!({
            "ordinal": 2, "timestamp": "2026-11-01T10:00:02.000Z", "type": "response_item",
            "payload": { "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "OLD_SENTINEL" }] }
        })
        .to_string(),
    );
    lines.push(
        json!({
            "ordinal": 3, "timestamp": "2026-11-01T10:00:03.000Z", "type": "response_item",
            "payload": { "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "z".repeat(6000) }] }
        })
        .to_string(),
    );
    let original = transcript_text(&lines);
    write_text(&path, &original);
    let source = fixture.add_source("codex");
    fixture.scan(&source);

    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    // Exactly the same byte length, and the mtime is put back: neither size,
    // mtime, head hash nor the 4 KiB anchor can see this.
    write_text(&path, &original.replace("OLD_SENTINEL", "NEW_SENTINEL"));
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    fixture.scan(&source);
    let conn = fixture.service.store().connection().lock();
    let new: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE content_json LIKE '%NEW_SENTINEL%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(new, 1, "the middle rewrite must be re-imported");
}

#[test]
fn an_unchanged_file_is_verified_without_reimporting() {
    let fixture = Fixture::new("r1-verify-idempotent");
    let session = "019a0000-0000-7000-8000-00000000r142";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r142.jsonl");
    write_text(&path, &transcript_text(&simple_codex(&fixture, session, 3)));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    let report = fixture.scan(&source);
    assert_eq!(report.progress.messages, 0);
    assert_eq!(report.progress.files_skipped, 1);
    assert_eq!(report.progress.files_verified, 1);
    assert!(report.progress.verified_bytes > 0);
    assert_eq!(fixture.message_count(), 3);
}

#[test]
fn an_atomic_replacement_swaps_the_transcript() {
    let fixture = Fixture::new("r1-atomic-replace");
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r143.jsonl");
    let first_session = "019a0000-0000-7000-8000-00000000r143";
    write_text(
        &path,
        &transcript_text(&simple_codex(&fixture, first_session, 3)),
    );
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 3);

    let second_session = "019a0000-0000-7000-8000-00000000r144";
    let replacement = fixture.root.path.join("replacement.tmp");
    write_text(
        &replacement,
        &transcript_text(&simple_codex(&fixture, second_session, 2)),
    );
    fs::rename(&replacement, &path).unwrap();
    fixture.scan(&source);
    let total = fixture.message_count();
    let conn = fixture.service.store().connection().lock();
    let stale: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE content_json LIKE '%消息 3%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);
    assert_eq!(stale, 0, "the replaced transcript's rows are gone");
    assert_eq!(total, 2);
}

#[test]
fn a_compressed_transcript_replacement_is_detected() {
    let fixture = Fixture::new("r1-zstd-replace");
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r145.jsonl.zst");
    let session = "019a0000-0000-7000-8000-00000000r145";
    write_zstd(&path, &transcript_text(&simple_codex(&fixture, session, 2)));
    let source = fixture.add_source("codex");
    let first = fixture.scan(&source);
    assert_eq!(first.progress.messages, 2);

    let mut replaced = simple_codex(&fixture, session, 2);
    replaced[1] = replaced[1].replace("消息 1", "改写后的消息");
    write_zstd(&path, &transcript_text(&replaced));
    fixture.scan(&source);
    let total = fixture.message_count();
    let conn = fixture.service.store().connection().lock();
    let updated: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE content_json LIKE '%改写后的消息%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);
    assert_eq!(updated, 1);
    assert_eq!(total, 2);
}

#[test]
fn an_mtime_rollback_with_growth_is_still_an_append() {
    let fixture = Fixture::new("r1-mtime-rollback");
    let session = "019a0000-0000-7000-8000-00000000r146";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r146.jsonl");
    let lines = simple_codex(&fixture, session, 2);
    write_text(&path, &transcript_text(&lines));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    let modified = fs::metadata(&path).unwrap().modified().unwrap();

    append_text(&path, &(lines[2].clone() + "\n"));
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    fixture.scan(&source);
    assert_eq!(
        fixture.message_count(),
        3,
        "a stale mtime must not hide an append"
    );
}

// --- R1-06 -----------------------------------------------------------------

#[test]
fn a_bad_first_line_does_not_reject_the_transcript() {
    let fixture = Fixture::new("r1-bad-first");
    let session = "019a0000-0000-7000-8000-00000000r151";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r151.jsonl");
    write_text(
        &path,
        &("{broken}\n".to_string() + &transcript_text(&simple_codex(&fixture, session, 2))),
    );
    let source = fixture.add_source("codex");
    let report = fixture.scan(&source);
    assert_eq!(fixture.message_count(), 2);
    assert_eq!(report.progress.line_errors, 1);
    assert!(
        !report.complete,
        "a pass with a rejected record is not complete"
    );
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "invalid-json")
        .expect("the bad line is reported");
    assert_eq!(issue.line, Some(1));
}

#[test]
fn a_wrong_product_file_is_still_rejected() {
    let fixture = Fixture::new("r1-wrong-product");
    let path = fixture.history.join("claude-as-codex.jsonl");
    let body = [
        json!({"type": "user", "sessionId": "abc", "message": {"role": "user", "content": "hi"}})
            .to_string(),
        json!({"type": "assistant", "sessionId": "abc", "message": {"role": "assistant", "content": "yo"}})
            .to_string(),
    ]
    .join("\n");
    write_text(&path, &(body + "\n"));
    let source = fixture.add_source("codex");
    let report = fixture.scan(&source);
    assert_eq!(fixture.message_count(), 0);
    assert_eq!(report.progress.files_failed, 1);
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.code == "unsupported"));
    assert!(!report.complete);
}

#[test]
fn line_and_oversized_diagnostics_reach_the_source_status() {
    let fixture = Fixture::new("r1-status-diagnostics");
    let session = "019a0000-0000-7000-8000-00000000r152";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r152.jsonl");
    let lines = simple_codex(&fixture, session, 1);
    write_text(&path, &(transcript_text(&lines) + "{broken}\n"));
    let source = fixture.add_source("codex");
    let engine = HistoryEngine::new(
        "local-windows",
        Arc::new(MultiCliService::open(fixture.root.path.join("data")).unwrap()),
    );
    engine.run_source_for_test(&source.id, false);
    let status = engine.status().unwrap();
    let error = status.sources[0]
        .status
        .error
        .as_ref()
        .expect("a bad row must not be swallowed as success");
    assert_eq!(status.sources[0].status.status, "partial");
    let issues = error.details["issues"]
        .as_array()
        .expect("issues are listed");
    assert!(issues
        .iter()
        .any(|issue| issue["code"] == "invalid-json" && issue["line"].as_u64() == Some(3)));
}

#[test]
fn an_oversized_record_is_isolated_and_reported() {
    let fixture = Fixture::new("r1-oversized");
    let session = "019a0000-0000-7000-8000-00000000r153";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r153.jsonl");
    let lines = simple_codex(&fixture, session, 1);
    let oversized = format!(
        "{{\"ordinal\":2,\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{}\"}}]}}}}",
        "y".repeat(model::MAX_LINE_BYTES + 1024)
    );
    write_text(
        &path,
        &(transcript_text(&lines) + &oversized + "\n" + &lines[1] + "\n"),
    );
    let source = fixture.add_source("codex");
    let report = fixture.scan(&source);
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.code == "line-too-large"));
    assert!(fixture.message_count() >= 2, "the surrounding rows survive");
    let conn = fixture.service.store().connection().lock();
    let marker: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE content_json LIKE '%oversized-line%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(marker, 1);
}

// --- R1-07 -----------------------------------------------------------------

#[test]
fn a_search_cursor_is_rejected_after_a_rebuild() {
    let fixture = Fixture::new("r1-cursor-epoch");
    let session = "019a0000-0000-7000-8000-00000000r161";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r161.jsonl");
    write_text(&path, &transcript_text(&simple_codex(&fixture, session, 4)));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    let mut query = HistorySearchQuery {
        text: "消息".into(),
        workspace_id: None,
        agent_id: None,
        source_id: None,
        limit: 1,
        cursor: None,
    };
    let first = query::search_history(&fixture.service, &query).unwrap();
    let cursor = first.next_cursor.clone().expect("more pages");

    {
        let mut conn = fixture.service.store().connection().lock();
        repo::purge_source_derived(&mut conn, &source.id).unwrap();
    }
    fixture.scan(&source);
    query.cursor = Some(cursor);
    let error = query::search_history(&fixture.service, &query).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
}

#[test]
fn a_message_cursor_is_rejected_for_another_session() {
    let fixture = Fixture::new("r1-message-cursor");
    for (name, session) in [
        ("a.jsonl", "019a0000-0000-7000-8000-00000000r162"),
        ("b.jsonl", "019a0000-0000-7000-8000-00000000r163"),
    ] {
        write_text(
            &fixture.history.join(name),
            &transcript_text(&simple_codex(&fixture, session, 3)),
        );
    }
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    let ids: Vec<String> = {
        let conn = fixture.service.store().connection().lock();
        let mut statement = conn
            .prepare("SELECT id FROM sessions ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(ids.len(), 2);
    let first = query::message_page(
        &fixture.service,
        &MessagePageQuery {
            session_id: ids[0].clone(),
            limit: 1,
            cursor: None,
            anchor_message_id: None,
        },
    )
    .unwrap();
    let error = query::message_page(
        &fixture.service,
        &MessagePageQuery {
            session_id: ids[1].clone(),
            limit: 1,
            cursor: first.next_cursor,
            anchor_message_id: None,
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
}

#[test]
fn paginating_one_query_is_stable_and_does_not_repeat_rows() {
    let fixture = Fixture::new("r1-paging-stable");
    let session = "019a0000-0000-7000-8000-00000000r164";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r164.jsonl");
    write_text(&path, &transcript_text(&simple_codex(&fixture, session, 5)));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    let mut query = HistorySearchQuery {
        text: "消息".into(),
        workspace_id: None,
        agent_id: None,
        source_id: None,
        limit: 2,
        cursor: None,
    };
    let mut seen = Vec::new();
    loop {
        let page = query::search_history(&fixture.service, &query).unwrap();
        assert_eq!(page.total, 5);
        seen.extend(page.items.iter().map(|hit| hit.message_id.clone()));
        match page.next_cursor {
            Some(cursor) => query.cursor = Some(cursor),
            None => break,
        }
    }
    assert_eq!(seen.len(), 5);
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 5, "no row is returned twice");
}
// --- Migration --------------------------------------------------------------

#[test]
fn schema_v5_migrates_a_populated_v4_database() {
    let root = TempRoot::new("v5-migration");
    {
        let service = MultiCliService::open_for_test(
            &root.path,
            MigrationFault {
                stop_after_version: Some(4),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(service.schema_version(), 4);
        service
            .register_host(&Host::local("local-windows"))
            .unwrap();
        service
            .register_agent(&AgentDefinition {
                id: "codex".into(),
                vendor: "OpenAI".into(),
                display_name: "Codex CLI".into(),
            })
            .unwrap();
        // Rows written by the v4 code path: one message with one origin, one
        // cursor, one user note.
        service
            .execute_raw(
                r#"
INSERT INTO sessions(id, agent_id, host_id, source_namespace, native_id, origin, title, created_at_ms)
VALUES ('s-v4', 'codex', 'local-windows', 'history:codex:codex:p', 'native-v4', 'history-only', 'kept title', 1700000000000);

INSERT INTO messages(id, session_id, run_id, native_event_id, source_key, seq, kind, content_json, source_ref, observed_at, created_at_ms)
VALUES ('m-v4', 's-v4', NULL, 'evt-1', 'codex:response:evt-1', 1, 'user-message', '{"text":"kept body"}', 'ref', '2026-11-01T10:00:00.000Z', 1700000000000);

INSERT INTO history_sources(id, host_id, agent_id, product, installation_id, config_profile_id, root_display_path, root_canonical_path, enabled, parser_version, created_at_ms, updated_at_ms)
VALUES ('hsrc-v4', 'local-windows', 'codex', 'codex', NULL, 'p', 'C:\History', 'C:\History', 1, 1, 1700000000000, 1700000000000);

INSERT INTO history_message_origins(message_id, source_id, file_identity, byte_offset, line_index, parser_version, created_at_ms)
VALUES ('m-v4', 'hsrc-v4', 'path:aaa', 0, 1, 1, 1700000000000);

INSERT INTO history_cursors(source_id, file_identity, display_path, canonical_path, size_bytes, mtime_ms, head_hash, anchor_hash, committed_offset, tail_residue, tail_residue_truncated, parser_version, error_count, last_error_json, updated_at_ms)
VALUES ('hsrc-v4', 'path:aaa', 'C:\History\a.jsonl', 'C:\History\a.jsonl', 100, 1700000000000, 'head', 'anchor', 50, '', 0, 1, 2, NULL, 1700000000000);

INSERT INTO user_session_notes(session_id, note, updated_at_ms) VALUES ('s-v4', 'note across v5', 1700000000000);
"#,
            )
            .unwrap();
    }

    let service = MultiCliService::open(&root.path).unwrap();
    assert_eq!(service.schema_version(), SCHEMA_VERSION);
    let conn = service.store().connection().lock();

    // Provenance survived, and the new primary key allows a second file of the
    // same source to contribute the same message.
    let origins: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM history_message_origins WHERE message_id = 'm-v4'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(origins, 1);
    conn.execute(
        "INSERT INTO history_message_origins(message_id, source_id, file_identity, byte_offset, line_index, parser_version, created_at_ms)
         VALUES ('m-v4', 'hsrc-v4', 'path:bbb', 10, 2, 1, 1700000000000)",
        [],
    )
    .unwrap();
    let origins: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM history_message_origins WHERE message_id = 'm-v4'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(origins, 2, "one message can be contributed by two files");

    // Old cursors keep their data and gain the honest defaults: unknown resume
    // state (`parser_state_json NULL`), not read to EOF, no line counter.
    let (lines, complete, state, verify): (i64, i64, Option<String>, i64) = conn
        .query_row(
            "SELECT lines_committed, read_complete, parser_state_json, verify_offset
             FROM history_cursors WHERE file_identity = 'path:aaa'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(lines, 0);
    assert_eq!(complete, 0);
    assert!(state.is_none());
    assert_eq!(verify, 0);

    // The new support tables exist.
    for table in ["history_file_blocks", "history_meta"] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "{table}");
    }
    let epoch: String = conn
        .query_row(
            "SELECT value FROM history_meta WHERE key = 'purgeEpoch'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(epoch, "0");

    // User-owned data is untouched and the message still reads back.
    let note: String = conn
        .query_row(
            "SELECT note FROM user_session_notes WHERE session_id = 's-v4'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(note, "note across v5");
    let body: String = conn
        .query_row(
            "SELECT content_json FROM messages WHERE id = 'm-v4'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(body.contains("kept body"));
    let violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn an_append_cannot_hide_a_rewrite_just_before_the_offset() {
    let fixture = Fixture::new("r1-append-with-rewrite");
    let session = "019a0000-0000-7000-8000-00000000r147";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r147.jsonl");
    let mut lines = simple_codex(&fixture, session, 0);
    lines.push(
        json!({
            "ordinal": 1, "timestamp": "2026-11-01T10:00:01.000Z", "type": "response_item",
            "payload": { "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "a".repeat(2000) }] }
        })
        .to_string(),
    );
    lines.push(
        json!({
            "ordinal": 2, "timestamp": "2026-11-01T10:00:02.000Z", "type": "response_item",
            "payload": { "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "OLD_SENTINEL" }] }
        })
        .to_string(),
    );
    write_text(&path, &transcript_text(&lines));
    let source = fixture.add_source("codex");
    fixture.scan(&source);

    // Rewrite the record immediately before the committed offset, keep the
    // length, and append: the append alone must not absorb the rewrite.
    let mut rewritten = transcript_text(&lines);
    rewritten = rewritten.replace("OLD_SENTINEL", "NEW_SENTINEL");
    fs::write(&path, &rewritten).unwrap();
    append_text(
        &path,
        &(json!({
            "ordinal": 3, "timestamp": "2026-11-01T10:00:03.000Z", "type": "response_item",
            "payload": { "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "THIRD" }] }
        })
        .to_string()
            + "\n"),
    );
    fixture.scan(&source);
    let conn = fixture.service.store().connection().lock();
    let new: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE content_json LIKE '%NEW_SENTINEL%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(new, 1, "the rewrite before the append offset must be seen");
}

#[test]
fn a_same_size_rewrite_with_a_new_mtime_is_verified() {
    let fixture = Fixture::new("r1-same-size-new-mtime");
    let session = "019a0000-0000-7000-8000-00000000r148";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r148.jsonl");
    let original = transcript_text(&simple_codex(&fixture, session, 2)).replace("消息 1", "OLD_MARKER");
    write_text(&path, &original);
    let source = fixture.add_source("codex");
    fixture.scan(&source);

    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    write_text(&path, &original.replace("OLD_MARKER", "NEW_MARKER"));
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(modified + std::time::Duration::from_secs(5)),
        )
        .unwrap();
    fixture.scan(&source);
    let conn = fixture.service.store().connection().lock();
    let new: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE content_json LIKE '%NEW_MARKER%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(new, 1);
}

#[test]
fn verification_budget_reaches_every_file_in_a_corpus() {
    let fixture = Fixture::new("r1-verify-fairness");
    for index in 0..6 {
        write_text(
            &fixture.history.join(format!(
                "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-0000000{index:02}.jsonl"
            )),
            &transcript_text(&simple_codex(
                &fixture,
                &format!("019a0000-0000-7000-8000-0000000{index:02}"),
                3,
            )),
        );
    }
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 18);

    // A budget that covers roughly two files per pass. With no fairness the
    // same first two files would be re-verified forever and the rest never.
    let options = ScanOptions {
        verify_budget_bytes: 4_096,
        ..ScanOptions::incremental()
    };
    for _ in 0..4 {
        fixture.scan_with(&source, options.clone());
    }
    let conn = fixture.service.store().connection().lock();
    let pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM history_cursors WHERE verified_at_ms = 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0, "every file must finish a verification cycle");
}

// --- audit follow-ups (second review) ---------------------------------------

#[test]
fn a_partial_tail_on_a_block_boundary_is_not_re_read_every_pass() {
    let fixture = Fixture::new("r1-residue-block");
    let session = "019a0000-0000-7000-8000-00000000r171";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r171.jsonl");
    let header =
        json!({"ordinal":0,"timestamp":"2026-11-01T10:00:00.000Z","type":"session_meta",
               "payload":{"session_id":session,"id":session,"cwd":fixture.workspace_path()}})
        .to_string()
            + "\n";
    let base = json!({
        "ordinal": 1, "timestamp": "2026-11-01T10:00:01.000Z", "type": "response_item",
        "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": ""}]}
    })
    .to_string()
        + "\n";
    let padding = (model::BLOCK_BYTES as usize)
        .checked_sub(header.len() + base.len())
        .expect("header + base fit in one block");
    let line = base.replace("\"text\":\"\"", &format!("\"text\":\"{}\"", "a".repeat(padding)));
    assert_eq!(header.len() + line.len(), model::BLOCK_BYTES as usize);
    // A committed prefix that ends exactly on a block boundary, followed by a
    // half-written record: verification must look at committed bytes only.
    write_text(&path, &(header + &line + "{\"partial\":"));
    let source = fixture.add_source("codex");

    let first = fixture.scan(&source);
    assert_eq!(first.progress.messages, 1);
    assert_eq!(first.progress.files_incomplete, 0);
    let second = fixture.scan(&source);
    assert_eq!(
        second.progress.messages, 0,
        "an unchanged file must not be re-imported"
    );
    assert_eq!(second.purged_messages, 0, "no purge for a clean verification");
    assert_eq!(fixture.message_count(), 1);
    let conn = fixture.service.store().connection().lock();
    let verified: i64 = conn
        .query_row("SELECT verified_at_ms FROM history_cursors", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(verified > 0, "the cycle finished");
}

#[test]
fn a_full_re_read_restarts_line_numbers_and_keeps_message_ids() {
    let fixture = Fixture::new("r1-full-line-numbers");
    let session = "019a0000-0000-7000-8000-00000000r172";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r172.jsonl");
    let lines = simple_codex(&fixture, session, 5);
    write_text(&path, &transcript_text(&lines));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    let before: Vec<String> = {
        let conn = fixture.service.store().connection().lock();
        let mut statement = conn.prepare("SELECT id FROM messages ORDER BY seq").unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(before.len(), 5);

    // Rewrite the head so the prefix hash changes and the file is read from 0.
    let rewritten = transcript_text(&lines).replace("消息 1", "改写 1");
    write_text(&path, &rewritten);
    fixture.scan(&source);
    let after: Vec<String> = {
        let conn = fixture.service.store().connection().lock();
        let mut statement = conn.prepare("SELECT id FROM messages ORDER BY seq").unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        before, after,
        "a full re-read must renumber from zero, not continue the old counter"
    );
}

#[test]
fn an_append_keeps_the_verification_cycle_progress() {
    let fixture = Fixture::new("r1-append-verify");
    let session = "019a0000-0000-7000-8000-00000000r173";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r173.jsonl");
    let lines = simple_codex(&fixture, session, 2);
    write_text(&path, &transcript_text(&lines));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    fixture.scan(&source); // completes a verification cycle
    let verified_before: i64 = {
        let conn = fixture.service.store().connection().lock();
        conn.query_row("SELECT verified_at_ms FROM history_cursors", [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    assert!(verified_before > 0);

    append_text(&path, &(lines[2].clone() + "\n"));
    fixture.scan(&source);
    let verified_after: i64 = {
        let conn = fixture.service.store().connection().lock();
        conn.query_row("SELECT verified_at_ms FROM history_cursors", [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    assert_eq!(
        verified_before, verified_after,
        "an append must not throw away the cycle that already covered the prefix"
    );
}

#[test]
fn a_moved_mtime_inside_the_cooldown_is_still_verified() {
    let fixture = Fixture::new("r1-cooldown-mtime");
    let session = "019a0000-0000-7000-8000-00000000r174";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r174.jsonl");
    let original = transcript_text(&simple_codex(&fixture, session, 2)).replace("消息 1", "OLD_MARKER");
    write_text(&path, &original);
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    fixture.scan(&source); // verified_at is now set; the cooldown is active

    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    write_text(&path, &original.replace("OLD_MARKER", "NEW_MARKER"));
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(
            fs::FileTimes::new().set_modified(modified + std::time::Duration::from_secs(3)),
        )
        .unwrap();
    fixture.scan(&source);
    let conn = fixture.service.store().connection().lock();
    let new: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE content_json LIKE '%NEW_MARKER%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(new, 1, "a moved mtime must bypass the verification cooldown");
}

#[test]
fn deleting_a_transcript_purges_what_it_contributed() {
    let fixture = Fixture::new("r1-deleted-file");
    for (name, session) in [
        ("a.jsonl", "019a0000-0000-7000-8000-00000000r175"),
        ("b.jsonl", "019a0000-0000-7000-8000-00000000r176"),
    ] {
        write_text(
            &fixture.history.join(name),
            &transcript_text(&simple_codex(&fixture, session, 3)),
        );
    }
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 6);

    fs::remove_file(fixture.history.join("a.jsonl")).unwrap();
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 3, "the deleted file's rows are gone");
    let conn = fixture.service.store().connection().lock();
    let origins: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM history_message_origins WHERE file_identity NOT IN
             (SELECT file_identity FROM history_cursors)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(origins, 0, "no provenance points at a file without a cursor");
}

#[test]
fn a_root_inside_a_protected_directory_is_refused_by_the_scan() {
    let fixture = Fixture::new("r1-protected-root");
    let nested = fixture.history.join(".aws");
    fs::create_dir_all(&nested).unwrap();
    write_text(
        &nested.join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r177.jsonl"),
        &transcript_text(&simple_codex(
            &fixture,
            "019a0000-0000-7000-8000-00000000r177",
            2,
        )),
    );
    let source = fixture.add_source_at("codex", &nested);
    let error = scan_source(
        ScanInput {
            service: &fixture.service,
            source: &source,
            options: ScanOptions::incremental(),
            cancel: &|| false,
        },
        |_| {},
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::AccessDenied);
    assert_eq!(error.details["reason"], "protected-directory");
    assert_eq!(fixture.message_count(), 0);
}

#[test]
fn rejected_records_stay_reported_on_a_later_skipping_pass() {
    let fixture = Fixture::new("r1-line-error-persists");
    let session = "019a0000-0000-7000-8000-00000000r178";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r178.jsonl");
    write_text(
        &path,
        &(transcript_text(&simple_codex(&fixture, session, 2)) + "{broken}\n"),
    );
    let source = fixture.add_source("codex");
    let engine = HistoryEngine::new(
        "local-windows",
        Arc::new(MultiCliService::open(fixture.root.path.join("data")).unwrap()),
    );
    engine.run_source_for_test(&source.id, false);
    let first = engine.status().unwrap();
    assert_eq!(first.sources[0].status.status, "partial");

    // The file is unchanged, so the next pass skips it — the rejected record
    // must still be visible instead of flipping the source back to `ok`.
    engine.run_source_for_test(&source.id, false);
    let second = engine.status().unwrap();
    assert_eq!(second.sources[0].status.status, "partial");
    let error = second.sources[0]
        .status
        .error
        .as_ref()
        .expect("an earlier rejection is still a real error");
    assert_eq!(error.details["reason"], "line-errors");
    assert!(error.details["lineErrors"].as_u64().unwrap_or(0) >= 1);
}

#[test]
fn a_duplicate_event_keeps_the_first_stored_body_in_the_search_index() {
    let fixture = Fixture::new("r1-fts-mirror");
    let session = "019a0000-0000-7000-8000-00000000r179";
    let mut first = simple_codex(&fixture, session, 1);
    first[1] = first[1].replace("消息 1", "ALPHA_MARKER");
    let mut second = first.clone();
    second[1] = second[1].replace("ALPHA_MARKER", "BETA_MARKER");
    write_text(&fixture.history.join("a.jsonl"), &transcript_text(&first));
    write_text(&fixture.history.join("b.jsonl"), &transcript_text(&second));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    let search = |text: &str| {
        query::search_history(
            &fixture.service,
            &HistorySearchQuery {
                text: text.into(),
                workspace_id: None,
                agent_id: None,
                source_id: None,
                limit: 10,
                cursor: None,
            },
        )
        .unwrap()
        .total
    };
    assert_eq!(search("ALPHA_MARKER"), 1);
    assert_eq!(
        search("BETA_MARKER"),
        0,
        "the stored body is the first one; the index must not advertise the other"
    );
}

#[test]
fn an_unsupported_file_is_not_downgraded_by_an_earlier_bad_line() {
    let fixture = Fixture::new("r1-code-priority");
    // `a-bad.jsonl` sorts first and reports a line error; `b-claude.jsonl` is
    // the wrong product. The status code must stay `unsupported`.
    let session = "019a0000-0000-7000-8000-00000000r180";
    write_text(
        &fixture.history.join("a-bad.jsonl"),
        &(transcript_text(&simple_codex(&fixture, session, 1)) + "{broken}\n"),
    );
    write_text(
        &fixture.history.join("b-claude.jsonl"),
        &(json!({"type": "user", "sessionId": "abc", "message": {"role": "user", "content": "hi"}})
            .to_string()
            + "\n"),
    );
    let source = fixture.add_source("codex");
    let engine = HistoryEngine::new(
        "local-windows",
        Arc::new(MultiCliService::open(fixture.root.path.join("data")).unwrap()),
    );
    engine.run_source_for_test(&source.id, false);
    let status = engine.status().unwrap();
    let error = status.sources[0].status.error.as_ref().expect("error");
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert_eq!(error.retryable, false);
}


#[test]
fn full_reread_recomputes_the_first_message_title() {
    let fixture = Fixture::new("sol-r1-full-title");
    let path = fixture.history.join("renamed.jsonl");
    let header = json!({"type": "session_meta", "payload": {"id": "sol-full-title"}}).to_string();
    let message = |title: &str| json!({
        "type": "response_item", "timestamp": "2026-11-02T00:00:01Z",
        "payload": {"type": "message", "id": "same-event", "role": "user",
            "content": [{"type": "input_text", "text": title}]}
    }).to_string();
    write_lines(&path, &[header.clone(), message("OLD_TITLE")]);
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    write_lines(&path, &[header, message("NEW_TITLE")]);
    fixture.scan(&source);
    let conn = fixture.service.store().connection().lock();
    let title: String = conn.query_row("SELECT title FROM sessions", [], |row| row.get(0)).unwrap();
    assert_eq!(title, "NEW_TITLE", "a full read must not restore the old title_seen checkpoint");
}
// ---------------------------------------------------------------------------
// S03-R2: verification under continuous append, and compressed physical blocks
// ---------------------------------------------------------------------------

/// One Codex user message line with an explicit payload, so a test can control
/// its exact byte length.
fn codex_user_line(index: usize, text: &str) -> String {
    json!({
        "ordinal": index,
        "timestamp": format!("2026-11-01T10:00:{:02}.000Z", index.min(59)),
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": text }]
        }
    })
    .to_string()
}

fn fixture_containing(fixture: &Fixture, text: &str) -> i64 {
    let conn = fixture.service.store().connection().lock();
    conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE content_json LIKE ?1",
        [format!("%{text}%")],
        |row| row.get(0),
    )
    .unwrap()
}

/// Passes a zstd frame padded with a legal skippable frame to an exact size.
///
/// The point is to build two transcripts with the *same physical size*, so the
/// only way to tell them apart is the physical block baseline — which is what
/// a compressed file must rely on.
fn write_zstd_fixed_size(path: &std::path::Path, text: &str, size: usize) {
    use std::io::Write;
    let mut frame = Vec::new();
    {
        let mut encoder = zstd::stream::Encoder::new(&mut frame, 1).unwrap();
        encoder.include_checksum(true).unwrap();
        encoder.write_all(text.as_bytes()).unwrap();
        encoder.finish().unwrap();
    }
    assert!(
        frame.len() + 8 <= size,
        "the frame ({} B) does not fit the requested {size} B",
        frame.len()
    );
    let pad = size - frame.len() - 8;
    frame.extend_from_slice(&0x184D_2A50_u32.to_le_bytes());
    frame.extend_from_slice(&(pad as u32).to_le_bytes());
    frame.resize(size, 0);
    fs::write(path, frame).unwrap();
}

#[test]
fn a_continuously_appending_file_still_verifies_older_blocks() {
    let fixture = Fixture::new("r2-append-verify");
    let session = "019a0000-0000-7000-8000-00000000r201";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r201.jsonl");
    // ~1.8 MB across 30 records: the sentinel sits past the first verification
    // window, so a single window cannot find it.
    let mut lines = simple_codex(&fixture, session, 0);
    for index in 1..=30 {
        let text = if index == 25 {
            format!("OLD_SENTINEL{}", "x".repeat(60_000))
        } else {
            format!("filler {index} {}", "x".repeat(60_000))
        };
        lines.push(codex_user_line(index, &text));
    }
    write_text(&path, &transcript_text(&lines));
    // A second transcript proves the rebuild keeps the rest of the source.
    let other_session = "019a0000-0000-7000-8000-00000000r202";
    let other_path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r202.jsonl");
    write_text(
        &other_path,
        &transcript_text(&simple_codex(&fixture, other_session, 3)),
    );
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 33);
    assert_eq!(fixture_containing(&fixture, "OLD_SENTINEL"), 1);

    // Same byte length, new content: only the block comparison can see it.
    let rewritten = transcript_text(&lines).replace("OLD_SENTINEL", "NEW_SENTINEL");
    assert_eq!(rewritten.len(), transcript_text(&lines).len());
    write_text(&path, &rewritten);

    // One window per pass, and every pass also appends a record: the exact
    // shape that used to starve the prefix forever.
    let options = ScanOptions {
        verify_budget_bytes: model::VERIFY_WINDOW_BYTES,
        budget_ms: 30_000,
        max_bytes_per_file: u64::MAX,
        ..ScanOptions::incremental()
    };
    let mut detected_on = None;
    for round in 0..6 {
        append_text(&path, &(codex_user_line(31 + round, "appended") + "\n"));
        let report = fixture.scan_with(&source, options.clone());
        assert!(
            report.progress.files_done >= 1,
            "round {round}: the append must still be imported"
        );
        if fixture_containing(&fixture, "NEW_SENTINEL") == 1 {
            detected_on = Some(round);
            break;
        }
    }
    assert_eq!(
        detected_on,
        Some(1),
        "the sentinel sits in the second window: one window per pass means it must be found on the second pass"
    );
    assert_eq!(
        fixture_containing(&fixture, "OLD_SENTINEL"),
        0,
        "the rebuild must replace the old rows, not add to them"
    );
    assert_eq!(
        fixture_containing(&fixture, "filler 1 "),
        1,
        "a rebuild must not duplicate untouched records"
    );
    assert_eq!(
        fixture_containing(&fixture, "消息 1"),
        1,
        "the other transcript of the same source must survive the rebuild"
    );
    let sessions: i64 = {
        let conn = fixture.service.store().connection().lock();
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(sessions, 2, "session identity must survive the rebuild");
}

#[test]
fn a_compressed_transcript_rewritten_in_place_is_found_through_physical_blocks() {
    let fixture = Fixture::new("r2-compressed-inplace");
    let session = "019a0000-0000-7000-8000-00000000r211";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r211.jsonl.zst");
    let mut lines = simple_codex(&fixture, session, 0);
    for index in 1..=6 {
        let text = if index == 4 {
            format!("OLD_SENTINEL{}", "q".repeat(40_000))
        } else {
            format!("payload {index} {}", "q".repeat(40_000))
        };
        lines.push(codex_user_line(index, &text));
    }
    let original = transcript_text(&lines);
    // Both versions must be written at one physical size, and the mtime must be
    // restored, so size + mtime + prefix hash all say "unchanged".
    let size = 200_000;
    write_zstd_fixed_size(&path, &original, size);
    let source = fixture.add_source("codex");
    let first = fixture.scan(&source);
    assert!(first.complete, "fixture must import cleanly: {first:?}");
    assert_eq!(fixture.message_count(), 6);

    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let rewritten = original.replace("OLD_SENTINEL", "NEW_SENTINEL");
    assert_eq!(rewritten.len(), original.len());
    write_zstd_fixed_size(&path, &rewritten, size);
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), size as u64);
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        modified,
        "the restored mtime must round-trip exactly, or this test would pass through the mtime path"
    );

    let report = fixture.scan(&source);
    assert_eq!(report.progress.files_failed, 0);
    assert_eq!(
        report.progress.files_skipped, 1,
        "phase 1 must treat it as unchanged; the rebuild must come from the blocks"
    );
    assert_eq!(
        fixture_containing(&fixture, "NEW_SENTINEL"),
        1,
        "a compressed file is verified on its physical bytes; a same-size,\
         same-mtime rewrite must be rebuilt: {report:?}"
    );
    assert_eq!(fixture_containing(&fixture, "OLD_SENTINEL"), 0);
    assert_eq!(fixture.message_count(), 6);

    // The rebuilt rows must also be what search sees.
    let page = query::search_history(
        &fixture.service,
        &HistorySearchQuery {
            text: "NEW_SENTINEL".into(),
            workspace_id: None,
            agent_id: None,
            source_id: None,
            limit: 10,
            cursor: None,
        },
    )
    .unwrap();
    assert_eq!(page.total, 1, "FTS must follow the rebuilt rows");
}

#[test]
fn compressed_verification_advances_window_by_window() {
    let fixture = Fixture::new("r2-compressed-windows");
    let session = "019a0000-0000-7000-8000-00000000r212";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r212.jsonl.zst");
    // Poorly compressible payload: the physical file spans several 256 KiB
    // blocks, which is what makes a bounded window meaningful.
    let mut lines = simple_codex(&fixture, session, 0);
    let mut state = 0x1234_5678_u64;
    for index in 1..=8 {
        let mut text = String::with_capacity(200_000);
        while text.len() < 200_000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            text.push(char::from(b'a' + ((state >> 33) % 26) as u8));
        }
        lines.push(codex_user_line(index, &text));
    }
    write_zstd(&path, &transcript_text(&lines));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    let physical = fs::metadata(&path).unwrap().len();
    assert!(
        physical > model::BLOCK_BYTES,
        "the fixture must span more than one physical block (got {physical} B)"
    );
    let blocks: i64 = {
        let conn = fixture.service.store().connection().lock();
        conn.query_row(
            "SELECT COUNT(*) FROM history_file_blocks WHERE layer = 'physical'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert!(blocks >= 2, "the physical baseline must be blocked");

    // One window per pass: the cycle must still finish, and every pass must
    // have moved it forward.
    let options = ScanOptions {
        verify_budget_bytes: model::BLOCK_BYTES,
        budget_ms: 30_000,
        ..ScanOptions::incremental()
    };
    let passes = (physical / model::BLOCK_BYTES) as usize + 3;
    let mut progressed = 0;
    for _ in 0..passes {
        let report = fixture.scan_with(&source, options.clone());
        if report.progress.verified_bytes > 0 {
            progressed += 1;
        }
    }
    assert!(progressed >= 2, "verification must advance every pass");
    let verified: i64 = {
        let conn = fixture.service.store().connection().lock();
        conn.query_row("SELECT verified_at_ms FROM history_cursors", [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    assert!(verified > 0, "the physical cycle must complete");
}

#[test]
fn a_compressed_cursor_without_a_physical_baseline_is_rebuilt_once() {
    let fixture = Fixture::new("r2-compressed-baseline");
    let session = "019a0000-0000-7000-8000-00000000r213";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r213.jsonl.zst");
    write_zstd(&path, &transcript_text(&simple_codex(&fixture, session, 4)));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    // A cursor written by a build that only had logical blocks: the physical
    // layer is missing and the file cannot be verified until one exists.
    {
        let conn = fixture.service.store().connection().lock();
        conn.execute("DELETE FROM history_file_blocks WHERE layer = 'physical'", [])
            .unwrap();
        conn.execute("UPDATE history_cursors SET verified_at_ms = 0", [])
            .unwrap();
    }
    let report = fixture.scan(&source);
    assert_eq!(report.progress.files_failed, 0);
    let physical: i64 = {
        let conn = fixture.service.store().connection().lock();
        conn.query_row(
            "SELECT COUNT(*) FROM history_file_blocks WHERE layer = 'physical'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert!(physical >= 1, "the baseline must be rebuilt");
    assert_eq!(
        fixture.message_count(),
        4,
        "the one-time rebuild must not duplicate rows"
    );
}

#[test]
fn an_oversized_compressed_transcript_is_refused_before_it_is_decoded() {
    let fixture = Fixture::new("r2-compressed-too-large");
    let session = "019a0000-0000-7000-8000-00000000r214";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r214.jsonl.zst");
    write_zstd(&path, &transcript_text(&simple_codex(&fixture, session, 2)));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 2);

    // Grow the file past the accepted physical size without writing the data:
    // the check must happen before the decoder ever runs.
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(model::MAX_COMPRESSED_PHYSICAL_BYTES + 1)
        .unwrap();
    let report = fixture.scan(&source);
    assert!(!report.complete);
    assert_eq!(report.progress.files_failed, 1);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "unsupported" && issue.detail == "compressed-too-large"),
        "the refusal must name its reason: {:?}",
        report.issues
    );
    assert_eq!(
        fixture.message_count(),
        2,
        "a refused file keeps the rows of its last successful import"
    );
}

#[test]
fn verification_rotation_prevents_one_large_file_from_starving_the_rest() {
    let fixture = Fixture::new("r2-verify-rotation");
    // A needs three windows; B fits in one. With a one-window budget and no
    // rotation, A takes the window on every pass and B is never verified.
    let large_session = "019a0000-0000-7000-8000-00000000r221";
    let large_path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r221.jsonl");
    let mut large = simple_codex(&fixture, large_session, 0);
    for index in 1..=12 {
        large.push(codex_user_line(index, &"y".repeat(230_000)));
    }
    write_text(&large_path, &transcript_text(&large));
    let small_session = "019a0000-0000-7000-8000-00000000r222";
    let small_path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r222.jsonl");
    write_text(
        &small_path,
        &transcript_text(&simple_codex(&fixture, small_session, 3)),
    );
    let source = fixture.add_source("codex");
    fixture.scan(&source);

    let options = ScanOptions {
        verify_budget_bytes: model::VERIFY_WINDOW_BYTES,
        budget_ms: 30_000,
        ..ScanOptions::incremental()
    };
    for _ in 0..2 {
        fixture.scan_with(&source, options.clone());
    }
    let small_verified: i64 = {
        let conn = fixture.service.store().connection().lock();
        conn.query_row(
            "SELECT verified_at_ms FROM history_cursors WHERE canonical_path = ?1",
            [crate::multicli::path::canonical_text(
                &fs::canonicalize(&small_path).unwrap(),
            )],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert!(
        small_verified > 0,
        "the second file must get a turn while the large one is mid-cycle"
    );
}
// --- S03-R2: migration, restart rotation, multi-frame compressed input ------

fn zstd_frame(text: &str) -> Vec<u8> {
    use std::io::Write;
    let mut frame = Vec::new();
    {
        let mut encoder = zstd::stream::Encoder::new(&mut frame, 1).unwrap();
        encoder.include_checksum(true).unwrap();
        encoder.write_all(text.as_bytes()).unwrap();
        encoder.finish().unwrap();
    }
    frame
}

/// Writes raw bytes padded with a legal skippable frame to an exact size.
fn write_bytes_padded(path: &std::path::Path, bytes: &[u8], size: usize) {
    assert!(
        bytes.len() + 8 <= size,
        "payload ({} B) does not fit the requested {size} B",
        bytes.len()
    );
    let mut out = bytes.to_vec();
    let pad = size - out.len() - 8;
    out.extend_from_slice(&0x184D_2A50_u32.to_le_bytes());
    out.extend_from_slice(&(pad as u32).to_le_bytes());
    out.resize(size, 0);
    fs::write(path, out).unwrap();
}

#[test]
fn a_compressed_transcript_with_several_frames_is_verified_as_one_file() {
    let fixture = Fixture::new("r2-compressed-frames");
    let session = "019a0000-0000-7000-8000-00000000r215";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r215.jsonl.zst");
    let mut lines = simple_codex(&fixture, session, 0);
    for index in 1..=6 {
        let text = if index == 5 {
            format!("OLD_SENTINEL{}", "m".repeat(30_000))
        } else {
            format!("frame {index} {}", "m".repeat(30_000))
        };
        lines.push(codex_user_line(index, &text));
    }
    let whole = transcript_text(&lines);
    let (first_half, second_half) = whole.split_at(whole.len() / 2);
    let original = [zstd_frame(first_half), zstd_frame(second_half)].concat();

    let rewritten = whole.replace("OLD_SENTINEL", "NEW_SENTINEL");
    let (first_new, second_new) = rewritten.split_at(rewritten.len() / 2);
    let replacement = [zstd_frame(first_new), zstd_frame(second_new)].concat();

    let size = original.len().max(replacement.len()) + 4_096;
    write_bytes_padded(&path, &original, size);
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(
        fixture.message_count(),
        6,
        "a multi-frame stream is one transcript"
    );
    assert_eq!(fixture_containing(&fixture, "OLD_SENTINEL"), 1);

    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    write_bytes_padded(&path, &replacement, size);
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    fixture.scan(&source);
    assert_eq!(
        fixture_containing(&fixture, "NEW_SENTINEL"),
        1,
        "the physical baseline covers every frame, including the second one"
    );
    assert_eq!(fixture_containing(&fixture, "OLD_SENTINEL"), 0);
    assert_eq!(fixture.message_count(), 6);
}

#[test]
fn the_verification_rotation_survives_a_restart() {
    let mut fixture = Fixture::new("r2-rotation-restart");
    for index in 0..4 {
        let session = format!("019a0000-0000-7000-8000-00000000r23{index}");
        let path = fixture.history.join(format!(
            "rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r23{index}.jsonl"
        ));
        let mut lines = simple_codex(&fixture, &session, 0);
        for line in 1..=6 {
            lines.push(codex_user_line(line, &"z".repeat(120_000)));
        }
        write_text(&path, &transcript_text(&lines));
    }
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    let options = ScanOptions {
        verify_budget_bytes: model::BLOCK_BYTES,
        budget_ms: 30_000,
        ..ScanOptions::incremental()
    };
    let read_position = |fixture: &Fixture| -> Option<String> {
        let conn = fixture.service.store().connection().lock();
        conn.query_row(
            "SELECT value FROM history_meta WHERE key = ?1",
            [format!("verifyPath:{}", source.id)],
            |row| row.get(0),
        )
        .ok()
    };
    fixture.scan_with(&source, options.clone());
    let before = read_position(&fixture);
    assert!(before.is_some(), "a verified pass must record its position");

    fixture.restart();
    assert_eq!(
        read_position(&fixture),
        before,
        "a restart must not send verification back to the head of the listing"
    );
    for _ in 0..12 {
        fixture.scan_with(&source, options.clone());
    }
    let pending: i64 = {
        let conn = fixture.service.store().connection().lock();
        conn.query_row(
            "SELECT COUNT(*) FROM history_cursors WHERE verified_at_ms = 0",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(pending, 0, "every file must finish its cycle after a restart");
}

#[test]
fn schema_v6_migrates_a_populated_v5_database() {
    let root = TempRoot::new("v6-migration");
    {
        let service = MultiCliService::open_for_test(
            &root.path,
            MigrationFault {
                stop_after_version: Some(5),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(service.schema_version(), 5);
        service
            .register_host(&Host::local("local-windows"))
            .unwrap();
        service
            .register_agent(&AgentDefinition {
                id: "codex".into(),
                vendor: "OpenAI".into(),
                display_name: "Codex CLI".into(),
            })
            .unwrap();
        service
            .execute_raw(
                r#"
INSERT INTO sessions(id, agent_id, host_id, source_namespace, native_id, origin, title, created_at_ms)
VALUES ('s-v5', 'codex', 'local-windows', 'history:codex:codex:p', 'native-v5', 'history-only', 'v5 title', 1700000000000);

INSERT INTO history_sources(id, host_id, agent_id, product, installation_id, config_profile_id, root_display_path, root_canonical_path, enabled, parser_version, created_at_ms, updated_at_ms)
VALUES ('hsrc-v5', 'local-windows', 'codex', 'codex', NULL, 'p', 'C:\History', 'C:\History', 1, 1, 1700000000000, 1700000000000);

INSERT INTO history_file_blocks(source_id, file_identity, block_index, hash, bytes)
VALUES ('hsrc-v5', 'path:aaa', 0, 'logical-hash-0', 262144);

INSERT INTO history_cursors(source_id, file_identity, display_path, canonical_path, size_bytes, mtime_ms, head_hash, anchor_hash, committed_offset, tail_residue, tail_residue_truncated, parser_version, error_count, last_error_json, lines_committed, read_complete, parser_state_json, verify_offset, verified_at_ms, updated_at_ms)
VALUES ('hsrc-v5', 'path:aaa', 'C:\History\a.jsonl', 'C:\History\a.jsonl', 524288, 1700000000000, 'head', 'anchor', 524288, '', 0, 1, 0, NULL, 10, 1, NULL, 65536, 1700000000001, 1700000000000);
"#,
            )
            .unwrap();
    }

    let service = MultiCliService::open(&root.path).unwrap();
    assert_eq!(service.schema_version(), SCHEMA_VERSION);
    let conn = service.store().connection().lock();

    // The v5 block survives as the logical layer, and the same index can now
    // also carry a physical hash.
    let (layer, hash, bytes): (String, String, i64) = conn
        .query_row(
            "SELECT layer, hash, bytes FROM history_file_blocks
             WHERE file_identity = 'path:aaa' AND block_index = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(layer, "logical");
    assert_eq!(hash, "logical-hash-0");
    assert_eq!(bytes, 262144);
    conn.execute(
        "INSERT INTO history_file_blocks(source_id, file_identity, layer, block_index, hash, bytes)
         VALUES ('hsrc-v5', 'path:aaa', 'physical', 0, 'physical-hash-0', 4096)",
        [],
    )
    .unwrap();
    let physical: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM history_file_blocks WHERE layer = 'physical'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(physical, 1);

    // Cursor state is untouched by the rebuild.
    let (offset, verify, verified): (i64, i64, i64) = conn
        .query_row(
            "SELECT committed_offset, verify_offset, verified_at_ms
             FROM history_cursors WHERE file_identity = 'path:aaa'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(offset, 524288);
    assert_eq!(verify, 65536);
    assert_eq!(verified, 1700000000001);
}
// --- S03-R2: a cancel during verification leaves resumable state ------------

#[test]
fn a_cancel_during_verification_leaves_the_cycle_resumable() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    let fixture = Fixture::new("r2-cancel-verify");
    let session = "019a0000-0000-7000-8000-00000000r241";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r241.jsonl");
    let mut lines = simple_codex(&fixture, session, 0);
    for index in 1..=4 {
        lines.push(codex_user_line(index, &"c".repeat(230_000)));
    }
    write_text(&path, &transcript_text(&lines));
    let source = fixture.add_source("codex");
    fixture.scan(&source);

    // The first import-phase check passes, the phase-2 entry check passes, and
    // the third call lands inside the first block comparison.
    let calls = Arc::new(AtomicU64::new(0));
    let counter = calls.clone();
    let cancel = move || counter.fetch_add(1, Ordering::SeqCst) + 1 >= 3;
    let options = ScanOptions {
        budget_ms: 30_000,
        ..ScanOptions::incremental()
    };
    let report = scan_source(
        ScanInput {
            service: &fixture.service,
            source: &source,
            options,
            cancel: &cancel,
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(
        report.progress.verified_bytes, 0,
        "a cancelled window must not claim verified bytes"
    );
    assert!(
        report.cancelled,
        "a pass that stopped verifying before the cycle finished is cancelled"
    );
    assert_eq!(
        report.progress.files_failed, 0,
        "a cancelled window is not a file failure"
    );

    // The next pass has no cancel and must finish the cycle from the committed
    // offset rather than starting over or giving up.
    let report = fixture.scan(&source);
    assert_eq!(report.progress.files_failed, 0);
    assert!(report.progress.verified_bytes > 0);
    let verified: i64 = {
        let conn = fixture.service.store().connection().lock();
        conn.query_row("SELECT verified_at_ms FROM history_cursors", [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    assert!(verified > 0, "the cycle completes after the cancel is gone");
}
// --- S03-R2: verification must not hide an unimported tail -------------------

#[test]
fn verification_progress_never_hides_an_unimported_tail() {
    let fixture = Fixture::new("r2-verify-tail");
    let session = "019a0000-0000-7000-8000-00000000r251";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r251.jsonl");
    write_text(&path, &transcript_text(&simple_codex(&fixture, session, 3)));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    assert_eq!(fixture.message_count(), 3);

    // The file grows, and the verification phase records progress for the
    // prefix it compared — as it does for a file the import phase did not reach
    // in this pass.
    let appended = codex_user_line(4, "fourth message");
    append_text(&path, &(appended.clone() + "\n"));
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    {
        let conn = fixture.service.store().connection().lock();
        let cursor = repo::read_cursor_by_path(
            &conn,
            &source.id,
            &crate::multicli::path::canonical_text(&fs::canonicalize(&path).unwrap()),
        )
        .unwrap()
        .expect("the first pass wrote a cursor");
        repo::set_verify_progress(&conn, &source.id, &cursor.file_identity, 0, 1_700_000_000_000)
            .unwrap();
        // The imported length is what `decide` compares against; it must not
        // have been replaced by the newer listing length.
        let size: i64 = conn
            .query_row(
                "SELECT size_bytes FROM history_cursors WHERE file_identity = ?1",
                [cursor.file_identity.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(u64::try_from(size).unwrap(), cursor.size_bytes);
    }

    let report = fixture.scan(&source);
    assert_eq!(report.progress.files_failed, 0);
    assert_eq!(
        fixture.message_count(),
        4,
        "the appended record must still be imported after a verification write"
    );
    assert_eq!(fixture_containing(&fixture, "fourth message"), 1);
    // `modified` is only read to keep the mtime from being the same instant as
    // the first scan on coarse filesystems; the assertion above is the point.
    let _ = modified;
}

#[test]
fn an_oversized_compressed_transcript_keeps_reporting_its_error() {
    let fixture = Fixture::new("r2-compressed-too-large-repeat");
    let session = "019a0000-0000-7000-8000-00000000r252";
    let path = fixture
        .history
        .join("rollout-2026-11-01T10-00-00-019a0000-0000-7000-8000-00000000r252.jsonl.zst");
    write_zstd(&path, &transcript_text(&simple_codex(&fixture, session, 2)));
    let source = fixture.add_source("codex");
    fixture.scan(&source);
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(model::MAX_COMPRESSED_PHYSICAL_BYTES + 1)
        .unwrap();

    for _ in 0..2 {
        let report = fixture.scan(&source);
        assert!(!report.complete, "a refused file keeps the pass incomplete");
        assert_eq!(
            report.progress.files_failed, 1,
            "the refusal is counted once, not once per phase"
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "unsupported" && issue.detail == "compressed-too-large"),
            "the reason stays stable across passes: {:?}",
            report.issues
        );
        assert_eq!(fixture.message_count(), 2);
    }
}

#[test]
fn filtered_search_keeps_full_total_when_cursor_exhausts_the_page() {
    let fixture = Fixture::new("filtered-search-total");
    let source = seed_search(&fixture);
    let conn = fixture.service.store().connection().lock();
    let workspace: String = conn.query_row(
        "SELECT workspace_id FROM session_workspaces WHERE role = 'primary' LIMIT 1",
        [], |row| row.get(0),
    ).unwrap();
    // Two files may contribute the same message; source membership must not
    // duplicate either the total or the returned page.
    conn.execute(
        "INSERT INTO history_message_origins
         SELECT message_id, source_id, file_identity || '-copy', byte_offset,
                line_index, parser_version, created_at_ms FROM history_message_origins",
        [],
    ).unwrap();
    let filter = repo::SearchFilter {
        agent_id: Some("codex"), workspace_id: Some(&workspace), source_id: Some(&source.id),
    };
    for (text, expected) in [("中文", 2), ("历史记录", 1), ("recorder", 1), ("absent", 0)] {
        let mut cursor = None;
        let mut ids = Vec::new();
        loop {
            let (rows, total) = repo::search_messages(
                &conn, text, &filter, repo::match_mode_for(text), cursor.as_ref(), 1,
            ).unwrap();
            assert_eq!(total, expected, "total must precede cursor: {text}");
            assert!(rows.len() <= 1);
            let Some(row) = rows.first() else { break; };
            assert!(!ids.contains(&row.message.id), "page repeated a message");
            ids.push(row.message.id.clone());
            cursor = Some(model::SearchPosition {
                session_created_at: row.session_created_at,
                observed_at: row.message.observed_at.clone(),
                seq: row.message.seq, id: row.message.id.clone(),
            });
            assert!(ids.len() <= expected as usize);
        }
        assert_eq!(ids.len(), expected as usize);
    }
}
