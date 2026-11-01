//! Synthetic S01 navigation fixture.
//!
//! This is a developer tool, not a product command. It refuses `prod`, only
//! writes to an explicit `test` profile or a scratch/temp root, and seeds
//! deterministic `fixture-*` ids so cleanup can remove exactly what it
//! created without touching user rows.
//!
//! Usage (stop the app first):
//!   cargo run --locked --example multicli_fixture -- --profile test
//!   cargo run --locked --example multicli_fixture -- --profile test --clean
//!   cargo run --locked --example multicli_fixture -- --root D:\tmp\s01 --scratch
//!
//! The two workspaces are real Git worktrees with Chinese/space paths under
//! `<root>/fixture-workspaces`, so repo-group evidence comes from real Git
//! metadata rather than hand-written ids.

use ccgui_next_lib::multicli::{
    AgentDefinition, AgentInstallation, Capability, CapabilityEvidenceLevel, CapabilityName,
    CapabilityStatus, EvidenceLevel, ExecutionStatus, Host, HostPath, InteractionStatus, Message,
    MultiCliService, Run, RunOwnership, RunTransport, Session, SessionOrigin, SessionWorkspace,
    SessionWorkspaceRole,
};
use rusqlite::Connection;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const PROFILE_TEST: &str = "test";
const AGENT_CODEX: &str = "fixture-codex";
const AGENT_CLAUDE: &str = "fixture-claude";
const AGENT_CODEBUDDY: &str = "fixture-codebuddy";
const CROSS_SESSION: &str = "fixture-session-cross";
const CLAUDE_SESSION: &str = "fixture-session-claude";
const CODEBUDDY_SESSION: &str = "fixture-session-codebuddy";

#[derive(Debug, Default)]
struct Args {
    profile: Option<String>,
    root: Option<PathBuf>,
    scratch: bool,
    clean: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("multicli_fixture: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args(std::env::args().skip(1).collect())?;
    let root = resolve_root(&args)?;
    let service = MultiCliService::open(&root).map_err(|error| {
        format!(
            "cannot open MultiCLI database at {}: {}",
            root.display(),
            error
        )
    })?;

    if args.clean {
        let deleted = cleanup(&root)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "action": "clean",
                "root": root.display().to_string(),
                "deleted": deleted,
                "fixtureDirectory": fixture_directory(&root).display().to_string(),
                "note": "Only fixture-* rows were deleted. The fixture-workspaces directory is kept.",
            }))
            .map_err(|error| error.to_string())?
        );
        return Ok(());
    }

    let workspace = seed(&service, &root)?;
    let counts = json!({
        "workspaces": service.list_workspaces(&ccgui_next_lib::multicli::PageRequest::new(200, None))
            .map_err(|error| error.to_string())?.total,
        "agents": service.list_agents(&ccgui_next_lib::multicli::PageRequest::new(200, None))
            .map_err(|error| error.to_string())?.total,
        "sessions": service.list_sessions(&ccgui_next_lib::multicli::SessionQuery {
            workspace_id: None, agent_id: None, origin: None, limit: 200, cursor: None,
        }).map_err(|error| error.to_string())?.total,
        "crossSessionMessages": service.get_session(CROSS_SESSION).map_err(|error| error.to_string())?.message_count,
        "crossSessionRuns": service.get_session(CROSS_SESSION).map_err(|error| error.to_string())?.run_count,
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "seed",
            "fixtureVersion": 1,
            "root": root.display().to_string(),
            "profile": args.profile.clone().unwrap_or_else(|| "scratch".to_string()),
            "counts": counts,
            "workspaces": workspace,
            "crossWorkspaceSessionId": CROSS_SESSION,
            "cleanup": {
                "command": "cargo run --locked --example multicli_fixture -- --profile test --clean",
                "scope": "fixture-* rows created by this tool; existing user rows are not deleted",
                "files": fixture_directory(&root).display().to_string(),
            },
        }))
        .map_err(|error| error.to_string())?
    );
    Ok(())
}

fn parse_args(values: Vec<String>) -> Result<Args, String> {
    let mut args = Args::default();
    let mut index = 0;
    while index < values.len() {
        match values[index].as_str() {
            "--profile" => {
                index += 1;
                args.profile = Some(
                    values
                        .get(index)
                        .ok_or("--profile requires a value")?
                        .clone(),
                );
            }
            "--root" => {
                index += 1;
                args.root = Some(PathBuf::from(
                    values.get(index).ok_or("--root requires a value")?,
                ));
            }
            "--scratch" => args.scratch = true,
            "--clean" => args.clean = true,
            "--help" | "-h" => {
                return Err(
                    "usage: --profile test [--clean] | --root <absolute scratch dir> --scratch [--clean]"
                        .into(),
                )
            }
            other => return Err(format!("unknown argument: {other}")),
        }
        index += 1;
    }
    Ok(args)
}

fn resolve_root(args: &Args) -> Result<PathBuf, String> {
    if std::env::var("MULTICLI_PROFILE").ok().as_deref() == Some("prod") {
        return Err("refusing to seed: MULTICLI_PROFILE is prod".into());
    }
    match (&args.profile, &args.root) {
        (Some(profile), None) => {
            if profile != PROFILE_TEST {
                return Err(format!(
                    "refusing profile {profile}: fixture seeding is test-only and never prod"
                ));
            }
            let base =
                dirs::data_local_dir().ok_or("local application data directory unavailable")?;
            let parent = resolved_target(&base.join("MultiCLI"))?;
            let expected = parent.join(PROFILE_TEST);
            let actual = resolved_target(&expected)?;
            if !same_path(&actual, &expected) {
                return Err("test profile must not redirect through a junction".into());
            }
            Ok(actual)
        }
        (None, Some(root)) => {
            if !args.scratch {
                return Err("--root requires --scratch to acknowledge a synthetic root".into());
            }
            if !root.is_absolute() {
                return Err("--root must be absolute".into());
            }
            let root = resolved_target(root)?;
            if root.components().any(|part| {
                part.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(".ccgui-next")
            }) {
                return Err("refusing to write into .ccgui-next".into());
            }
            let temp = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| e.to_string())?;
            let production = dirs::data_local_dir()
                .map(|base| resolved_target(&base.join("MultiCLI/prod")))
                .transpose()?;
            if production
                .as_ref()
                .is_some_and(|prod| same_path(&root, prod) || root.starts_with(prod))
            {
                return Err("refusing production profile".into());
            }
            let is_scratch = root.components().any(|part| {
                part.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("scratch")
            }) || root.starts_with(&temp);
            if !is_scratch {
                return Err(
                    "scratch root must be under the system temp directory or a scratch folder"
                        .into(),
                );
            }
            Ok(root)
        }
        (Some(_), Some(_)) => Err("use either --profile or --root, not both".into()),
        (None, None) => {
            Err("must pass --profile test or --root <absolute scratch dir> --scratch".into())
        }
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

// Resolve before creating anything, including when the final directory does
// not exist. Lexical scratch names do not authorize a junction's target.
fn resolved_target(path: &Path) -> Result<PathBuf, String> {
    let mut existing = path;
    let mut suffix = Vec::new();
    while !existing.exists() {
        suffix.push(
            existing
                .file_name()
                .ok_or("path has no existing ancestor")?
                .to_os_string(),
        );
        existing = existing.parent().ok_or("path has no existing ancestor")?;
    }
    let mut resolved = std::fs::canonicalize(existing).map_err(|e| e.to_string())?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

fn fixture_directory(root: &Path) -> PathBuf {
    root.join("fixture-workspaces")
}

fn git(directory: &Path, arguments: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| format!("git is required for the fixture: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Creates the two real worktrees. Existing incomplete directories are preserved.
fn ensure_worktrees(root: &Path) -> Result<(PathBuf, PathBuf), String> {
    let base = fixture_directory(root);
    let main = base.join("Alpha 项目");
    let linked = base.join("Beta 工作区");
    if main.join(".git").is_file() || main.join(".git").is_dir() {
        if linked.join(".git").is_file() || linked.join(".git").is_dir() {
            return Ok((main, linked));
        }
    }
    if base.exists() {
        return Err(format!(
            "incomplete existing fixture directory preserved at {}; choose a fresh scratch root",
            base.display()
        ));
    }
    std::fs::create_dir_all(&main).map_err(|error| format!("create fixture repo: {error}"))?;
    git(&main, &["init"])?;
    git(
        &main,
        &[
            "-c",
            "user.name=MultiCLI Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "synthetic fixture",
        ],
    )?;
    // Git for Windows does not accept Rust canonicalize's verbatim prefix
    // for `worktree add`, even though std filesystem operations accept it.
    let linked_text = linked.to_str().ok_or("fixture path is not valid UTF-8")?;
    let linked_git = if let Some(unc) = linked_text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        linked_text.strip_prefix(r"\\?\").unwrap_or(linked_text).to_string()
    };
    git(
        &main,
        &[
            "worktree",
            "add",
            &linked_git,
            "-b",
            "fixture-worktree",
        ],
    )?;
    Ok((main, linked))
}

fn seed(service: &MultiCliService, root: &Path) -> Result<serde_json::Value, String> {
    service
        .register_host(&Host::local("local-windows"))
        .map_err(|error| error.to_string())?;

    for (id, vendor, display) in [
        (AGENT_CODEX, "OpenAI", "Codex"),
        (AGENT_CLAUDE, "Anthropic", "Claude"),
        (AGENT_CODEBUDDY, "Tencent", "CodeBuddy"),
    ] {
        service
            .register_agent(&AgentDefinition {
                id: id.into(),
                vendor: vendor.into(),
                display_name: display.into(),
            })
            .map_err(|error| error.to_string())?;
        service
            .register_installation(&AgentInstallation {
                id: format!("fixture-install-{id}"),
                host_id: "local-windows".into(),
                agent_id: id.into(),
                executable: HostPath {
                    host_id: "local-windows".into(),
                    display_path: format!(r"C:\MultiCLI-Fixture\{id}.exe"),
                    canonical_path: None,
                },
                version: Some("0.0.0-fixture".into()),
                config_profile_id: "default".into(),
            })
            .map_err(|error| error.to_string())?;
    }

    let (main, linked) = ensure_worktrees(root)?;
    let main_resolved =
        service.resolve_workspace_path("local-windows", &main.display().to_string());
    let linked_resolved =
        service.resolve_workspace_path("local-windows", &linked.display().to_string());
    if !main_resolved.is_resolved() || !linked_resolved.is_resolved() {
        return Err("fixture worktrees did not resolve to real directories".into());
    }
    if main_resolved.repo_group_id != linked_resolved.repo_group_id
        || main_resolved.repo_group_id.is_none()
    {
        return Err("fixture worktrees did not share real Git metadata".into());
    }
    let alpha = service
        .ensure_workspace_at_path("local-windows", &main.display().to_string())
        .map_err(|error| error.to_string())?;
    let beta = service
        .ensure_workspace_at_path("local-windows", &linked.display().to_string())
        .map_err(|error| error.to_string())?;

    for (session, agent, origin, title, native) in [
        (
            CROSS_SESSION,
            AGENT_CODEX,
            SessionOrigin::Managed,
            "跨工作区 · Alpha ↔ Beta",
            "fixture-native-cross",
        ),
        (
            CLAUDE_SESSION,
            AGENT_CLAUDE,
            SessionOrigin::ExternalObserved,
            "Claude 外部观察",
            "fixture-native-claude",
        ),
        (
            CODEBUDDY_SESSION,
            AGENT_CODEBUDDY,
            SessionOrigin::HistoryOnly,
            "CodeBuddy 历史记录",
            "fixture-native-codebuddy",
        ),
    ] {
        service
            .ensure_session(&Session {
                id: session.into(),
                agent_id: agent.into(),
                host_id: "local-windows".into(),
                source_namespace: format!("{agent}/fixture/local-windows"),
                native_id: Some(native.into()),
                origin,
                title: title.into(),
            })
            .map_err(|error| error.to_string())?;
    }

    service
        .attach_workspace(&SessionWorkspace {
            session_id: CROSS_SESSION.into(),
            workspace_id: alpha.id.clone(),
            role: SessionWorkspaceRole::Primary,
            evidence: "fixture: primary checkout".into(),
        })
        .map_err(|error| error.to_string())?;
    service
        .attach_workspace(&SessionWorkspace {
            session_id: CROSS_SESSION.into(),
            workspace_id: beta.id.clone(),
            role: SessionWorkspaceRole::Related,
            evidence: "fixture: linked worktree".into(),
        })
        .map_err(|error| error.to_string())?;
    service
        .attach_workspace(&SessionWorkspace {
            session_id: CLAUDE_SESSION.into(),
            workspace_id: alpha.id.clone(),
            role: SessionWorkspaceRole::Primary,
            evidence: "fixture: observed process cwd".into(),
        })
        .map_err(|error| error.to_string())?;
    service
        .attach_workspace(&SessionWorkspace {
            session_id: CODEBUDDY_SESSION.into(),
            workspace_id: beta.id.clone(),
            role: SessionWorkspaceRole::Primary,
            evidence: "fixture: history record".into(),
        })
        .map_err(|error| error.to_string())?;

    let runs = [
        (
            "fixture-run-1",
            CROSS_SESSION,
            RunTransport::Native,
            RunOwnership::Managed,
            ExecutionStatus::Exited,
            InteractionStatus::Idle,
            EvidenceLevel::Observed,
            Some("2026-11-01T09:00:00.000Z"),
            Some("2026-11-01T09:04:00.000Z"),
        ),
        (
            "fixture-run-2",
            CROSS_SESSION,
            RunTransport::Acp,
            RunOwnership::Managed,
            ExecutionStatus::Running,
            InteractionStatus::Working,
            EvidenceLevel::Observed,
            Some("2026-11-01T09:05:00.000Z"),
            None,
        ),
        (
            "fixture-run-3",
            CROSS_SESSION,
            RunTransport::Pty,
            RunOwnership::ExternalObserved,
            ExecutionStatus::Running,
            InteractionStatus::Unknown,
            EvidenceLevel::Inferred,
            Some("2026-11-01T09:06:00.000Z"),
            None,
        ),
        (
            "fixture-run-4",
            CLAUDE_SESSION,
            RunTransport::Native,
            RunOwnership::ExternalObserved,
            ExecutionStatus::Running,
            InteractionStatus::AwaitingPermission,
            EvidenceLevel::Observed,
            Some("2026-11-01T09:02:00.000Z"),
            None,
        ),
        (
            "fixture-run-5",
            CODEBUDDY_SESSION,
            RunTransport::Native,
            RunOwnership::ExternalObserved,
            ExecutionStatus::Exited,
            InteractionStatus::Idle,
            EvidenceLevel::Inferred,
            Some("2026-11-01T08:40:00.000Z"),
            Some("2026-11-01T08:50:00.000Z"),
        ),
    ];
    for (id, session, transport, ownership, execution, interaction, evidence, started, ended) in
        runs
    {
        service
            .start_run(&Run {
                id: id.into(),
                session_id: session.into(),
                installation_id: None,
                transport,
                ownership,
                started_at: started.map(str::to_string),
                ended_at: ended.map(str::to_string),
                execution_status: execution,
                interaction_status: interaction,
                evidence_level: evidence,
                evidence: Some("synthetic fixture run".into()),
            })
            .map_err(|error| error.to_string())?;
    }

    let messages = [
        (
            CROSS_SESSION,
            "fixture-run-1",
            "user",
            "把 Alpha 的改动同步到 Beta 工作区。",
        ),
        (
            CROSS_SESSION,
            "fixture-run-1",
            "assistant",
            "已读取两个 worktree 的 Git 元信息。",
        ),
        (
            CROSS_SESSION,
            "fixture-run-1",
            "tool",
            "git status --short  (fixture)",
        ),
        (
            CROSS_SESSION,
            "fixture-run-2",
            "assistant",
            "正在检查跨工作区链接。",
        ),
        (
            CROSS_SESSION,
            "fixture-run-2",
            "assistant",
            "同一个 Session 在两侧显示相同计数。",
        ),
        (
            CROSS_SESSION,
            "fixture-run-3",
            "assistant",
            "外部观察的 PTY Run 不拥有进程。",
        ),
        (
            CLAUDE_SESSION,
            "fixture-run-4",
            "assistant",
            "Claude 正在等待权限。",
        ),
        (
            CLAUDE_SESSION,
            "fixture-run-4",
            "tool",
            "权限请求仅展示，不自动回答。",
        ),
        (
            CLAUDE_SESSION,
            "fixture-run-4",
            "assistant",
            "能力证据与状态分开显示。",
        ),
        (
            CODEBUDDY_SESSION,
            "fixture-run-5",
            "assistant",
            "历史记录只读展示。",
        ),
        (
            CODEBUDDY_SESSION,
            "fixture-run-5",
            "assistant",
            "未执行真实历史扫描。",
        ),
    ];
    for (index, (session, run, kind, text)) in messages.iter().enumerate() {
        service
            .record_message(&Message {
                id: format!("fixture-msg-{}", index + 1),
                session_id: (*session).into(),
                run_id: Some((*run).into()),
                native_event_id: Some(format!("fixture-native-event-{}", index + 1)),
                source_key: format!("fixture:{}", index + 1),
                seq: index as u64,
                kind: format!("{kind}-message"),
                content: json!({ "role": kind, "text": text }),
                source_ref: Some("fixture".into()),
                observed_at: format!("2026-11-01T09:0{}:{:02}.000Z", index % 10, index % 60),
            })
            .map_err(|error| error.to_string())?;
    }

    let capabilities = [
        (
            AGENT_CODEX,
            CapabilityName::StructuredChat,
            CapabilityStatus::Supported,
            CapabilityEvidenceLevel::Live,
            None,
        ),
        (
            AGENT_CODEX,
            CapabilityName::HistoryRead,
            CapabilityStatus::Supported,
            CapabilityEvidenceLevel::Live,
            None,
        ),
        (
            AGENT_CODEX,
            CapabilityName::PermissionFlow,
            CapabilityStatus::Unverified,
            CapabilityEvidenceLevel::Fixture,
            Some("fixture only; live round trip NOT_RUN"),
        ),
        (
            AGENT_CODEX,
            CapabilityName::QuotaRead,
            CapabilityStatus::Unsupported,
            CapabilityEvidenceLevel::Document,
            Some("no official quota API"),
        ),
        (
            AGENT_CLAUDE,
            CapabilityName::StructuredChat,
            CapabilityStatus::Supported,
            CapabilityEvidenceLevel::Live,
            None,
        ),
        (
            AGENT_CLAUDE,
            CapabilityName::PermissionFlow,
            CapabilityStatus::Supported,
            CapabilityEvidenceLevel::Live,
            None,
        ),
        (
            AGENT_CLAUDE,
            CapabilityName::HistoryRead,
            CapabilityStatus::Unverified,
            CapabilityEvidenceLevel::Fixture,
            Some("fixture only; parser NOT_RUN"),
        ),
        (
            AGENT_CLAUDE,
            CapabilityName::QuotaRead,
            CapabilityStatus::Unsupported,
            CapabilityEvidenceLevel::Document,
            Some("no official quota API"),
        ),
        (
            AGENT_CODEBUDDY,
            CapabilityName::StructuredChat,
            CapabilityStatus::Unverified,
            CapabilityEvidenceLevel::Fixture,
            Some("fixture only; ACP not verified"),
        ),
        (
            AGENT_CODEBUDDY,
            CapabilityName::PermissionFlow,
            CapabilityStatus::Unverified,
            CapabilityEvidenceLevel::Fixture,
            Some("fixture only; permission round trip NOT_RUN"),
        ),
        (
            AGENT_CODEBUDDY,
            CapabilityName::QuotaRead,
            CapabilityStatus::Unsupported,
            CapabilityEvidenceLevel::Document,
            Some("no official quota API"),
        ),
    ];
    for (agent, name, status, evidence, reason) in capabilities {
        service
            .declare_capability(&Capability {
                agent_id: agent.into(),
                installation_id: None,
                name,
                status,
                version_range: Some("0.0.0-fixture".into()),
                evidence_level: evidence,
                source: "synthetic fixture; not a real CLI probe".into(),
                verified_at: None,
                reason: reason.map(str::to_string),
            })
            .map_err(|error| error.to_string())?;
    }

    Ok(json!([
        {
            "id": alpha.id,
            "displayPath": alpha.path.display_path,
            "canonicalPath": alpha.path.canonical_path,
            "repoGroupId": alpha.repo_group_id,
            "worktreeId": alpha.worktree_id,
            "role": "primary",
        },
        {
            "id": beta.id,
            "displayPath": beta.path.display_path,
            "canonicalPath": beta.path.canonical_path,
            "repoGroupId": beta.repo_group_id,
            "worktreeId": beta.worktree_id,
            "role": "related",
        },
    ]))
}

fn cleanup(root: &Path) -> Result<serde_json::Value, String> {
    let database = root.join("multicli.db");
    if !database.exists() {
        return Ok(json!({ "database": "absent", "rows": 0 }));
    }
    let conn = Connection::open(&database).map_err(|error| format!("open database: {error}"))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    let tx = conn
        .unchecked_transaction()
        .map_err(|error| format!("begin cleanup: {error}"))?;
    // Refuse the whole cleanup before its first DELETE when user-owned rows
    // depend on the synthetic parents. SQLite cascades would otherwise erase
    // those sessions, messages or runs while reporting a successful cleanup.
    let shared: i64 = tx.query_row(
        "SELECT
          (SELECT COUNT(*) FROM sessions WHERE id NOT LIKE 'fixture-%' AND agent_id LIKE 'fixture-%') +
          (SELECT COUNT(*) FROM runs WHERE id NOT LIKE 'fixture-%' AND (session_id LIKE 'fixture-%' OR installation_id LIKE 'fixture-%')) +
          (SELECT COUNT(*) FROM messages WHERE id NOT LIKE 'fixture-%' AND (session_id LIKE 'fixture-%' OR run_id LIKE 'fixture-%')) +
          (SELECT COUNT(*) FROM agent_installations WHERE id NOT LIKE 'fixture-%' AND agent_id LIKE 'fixture-%') +
          (SELECT COUNT(*) FROM user_session_notes WHERE session_id LIKE 'fixture-%') +
          (SELECT COUNT(*) FROM user_session_tags WHERE session_id LIKE 'fixture-%') +
          (SELECT COUNT(*) FROM session_relations WHERE
             (parent_session_id LIKE 'fixture-%' AND child_session_id NOT LIKE 'fixture-%') OR
             (child_session_id LIKE 'fixture-%' AND parent_session_id NOT LIKE 'fixture-%'))",
        [], |row| row.get(0)
    ).map_err(|e| e.to_string())?;
    if shared > 0 {
        return Err(
            "fixture rows are referenced by non-fixture data; cleanup refused without changes"
                .into(),
        );
    }
    let mut deleted = serde_json::Map::new();
    let statements = [
        (
            "runProcessLinks",
            "DELETE FROM run_process_links WHERE run_id LIKE 'fixture-%'",
        ),
        (
            "messages",
            "DELETE FROM messages WHERE session_id LIKE 'fixture-%'",
        ),
        ("runs", "DELETE FROM runs WHERE session_id LIKE 'fixture-%'"),
        (
            "sessionWorkspaces",
            "DELETE FROM session_workspaces WHERE session_id LIKE 'fixture-%'",
        ),
        ("sessions", "DELETE FROM sessions WHERE id LIKE 'fixture-%'"),
        (
            "capabilities",
            "DELETE FROM capabilities WHERE agent_id LIKE 'fixture-%'",
        ),
        (
            "installations",
            "DELETE FROM agent_installations WHERE id LIKE 'fixture-%'",
        ),
        ("agents", "DELETE FROM agents WHERE id LIKE 'fixture-%'"),
    ];
    for (label, sql) in statements {
        let count = tx
            .execute(sql, [])
            .map_err(|error| format!("cleanup {label}: {error}"))?;
        deleted.insert(label.to_string(), json!(count));
    }
    let fixture_dir = fixture_directory(root);
    let fixture_text =
        ccgui_next_lib::multicli::path::normalize_display_path(&fixture_dir.display().to_string())
            .map_err(|error| format!("normalize fixture directory: {}", error.reason))?;
    let escaped = fixture_text
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_");
    let fixture_prefix = format!("{escaped}{}%", std::path::MAIN_SEPARATOR);
    let workspaces = tx
        .execute(
            "DELETE FROM workspaces
             WHERE display_path LIKE ?1 ESCAPE '!'
               AND NOT EXISTS (
                   SELECT 1 FROM session_workspaces sw
                   JOIN sessions s ON s.id = sw.session_id
                   WHERE sw.workspace_id = workspaces.id
                     AND s.id NOT LIKE 'fixture-%'
               )",
            [fixture_prefix],
        )
        .map_err(|error| format!("cleanup workspaces: {error}"))?;
    deleted.insert("workspaces".to_string(), json!(workspaces));
    tx.commit()
        .map_err(|error| format!("commit cleanup: {error}"))?;
    Ok(json!({ "rows": deleted }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("multicli-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn database(root: &Path, session_id: &str) -> MultiCliService {
        let service = MultiCliService::open(root).unwrap();
        service
            .register_host(&Host::local("local-windows"))
            .unwrap();
        service
            .register_agent(&AgentDefinition {
                id: AGENT_CODEX.into(),
                vendor: "synthetic".into(),
                display_name: "Synthetic".into(),
            })
            .unwrap();
        service
            .ensure_session(&Session {
                id: session_id.into(),
                agent_id: AGENT_CODEX.into(),
                host_id: "local-windows".into(),
                source_namespace: "synthetic".into(),
                native_id: None,
                origin: SessionOrigin::HistoryOnly,
                title: "keep".into(),
            })
            .unwrap();
        service
    }

    #[test]
    fn shared_fixture_parents_cannot_cascade_delete_user_sessions() {
        let root = root();
        let service = database(&root, "user-session");
        assert!(cleanup(&root).is_err());
        assert_eq!(
            service.get_session("user-session").unwrap().session.title,
            "keep"
        );
        assert_eq!(
            service
                .list_agents(&ccgui_next_lib::multicli::PageRequest::new(10, None))
                .unwrap()
                .total,
            1
        );
    }

    #[test]
    fn cleanup_keeps_sibling_workspaces_outside_the_fixture_directory() {
        let root = root();
        let service = database(&root, CROSS_SESSION);
        let display = format!("{}-user", fixture_directory(&root).display());
        service
            .register_workspace(&ccgui_next_lib::multicli::Workspace {
                id: "user-workspace".into(),
                path: HostPath {
                    host_id: "local-windows".into(),
                    display_path: display.clone(),
                    canonical_path: Some(display),
                },
                repo_group_id: None,
                worktree_id: None,
            })
            .unwrap();
        cleanup(&root).unwrap();
        assert_eq!(
            service
                .list_workspaces(&ccgui_next_lib::multicli::PageRequest::new(10, None))
                .unwrap()
                .total,
            1
        );
        assert!(service.get_session(CROSS_SESSION).is_err());
    }

    #[test]
    fn incomplete_fixture_directory_is_preserved() {
        let root = root();
        let base = fixture_directory(&root);
        std::fs::create_dir_all(&base).unwrap();
        let sentinel = base.join("user-note.txt");
        std::fs::write(&sentinel, "keep").unwrap();
        assert!(ensure_worktrees(&root).is_err());
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "keep");
    }

    #[test]
    fn production_fixture_profile_is_always_rejected() {
        assert!(resolve_root(&Args {
            profile: Some("prod".into()),
            ..Args::default()
        })
        .is_err());
    }
}
