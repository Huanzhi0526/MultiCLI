//! Synthetic history performance harness (developer tool, not a product command).
//!
//! Generates `--sessions` transcript files with `--messages` messages each (plus
//! a `session_meta` line per file) under an explicit scratch directory, indexes
//! them with the production scanner, then measures warm-cache list and search
//! latency. All data is synthetic and stays in the directory the caller names.
//!
//! Usage:
//!   cargo run --release --locked --example multicli_history_bench -- \
//!     --dir D:\scratch\s03\bench --sessions 10000 --messages 10
//!
//! Sampling: 30 iterations per case after 3 warm-up iterations. p95 is the
//! nearest-rank percentile over the 30 samples (index ceil(0.95*30)-1). Timing
//! covers one synchronous IPC-equivalent call (SQL execution plus result
//! mapping), not React rendering or the Tauri bridge.

use ccgui_next_lib::multicli::history::model::{HistorySearchQuery, MessagePageQuery};
use ccgui_next_lib::multicli::history::scan::ScanOptions;
use ccgui_next_lib::multicli::history::{query, tooling};
use ccgui_next_lib::multicli::{
    Host, HostPath, MultiCliService, SessionQuery, Workspace,
};
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SAMPLES: usize = 30;
const WARMUP: usize = 3;

struct Args {
    dir: PathBuf,
    sessions: usize,
    messages: usize,
    /// Reuse an existing database in `--dir`: skip generation and resume the
    /// index. Lets a long benchmark be split across two bounded processes.
    reuse: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("multicli_history_bench: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if !args.dir.is_absolute() {
        return Err("--dir must be absolute".into());
    }
    if args.dir.join("multicli.db").exists() && !args.reuse {
        return Err("--dir already contains a MultiCLI database; use a fresh directory".into());
    }
    std::fs::create_dir_all(&args.dir).map_err(|error| error.to_string())?;
    let workspace_dir = args.dir.join("workspace");
    std::fs::create_dir_all(&workspace_dir).map_err(|error| error.to_string())?;

    let history_root = args.dir.join("sessions");
    let reuse = args.reuse && args.dir.join("multicli.db").exists();
    let generate_seconds = if reuse {
        std::fs::create_dir_all(&history_root).map_err(|error| error.to_string())?;
        0.0
    } else {
        eprintln!("generating {} sessions", args.sessions);
        let generate_started = Instant::now();
        std::fs::create_dir_all(&history_root).map_err(|error| error.to_string())?;
        let elapsed = generate(&args, &history_root, &workspace_dir)?;
        let seconds = generate_started.elapsed().as_secs_f64();
        eprintln!("generated {elapsed} messages");
        seconds
    };
    // On a reused directory the generator did not run in this process, so the
    // reported generated count is the number actually indexed (checked below).
    let total_messages = args.sessions * args.messages;

    let service = MultiCliService::open(&args.dir).map_err(|error| error.to_string())?;
    service
        .register_host(&Host::local("local-windows"))
        .map_err(|error| error.to_string())?;
    let canonical_workspace = std::fs::canonicalize(&workspace_dir)
        .map_err(|error| error.to_string())?
        .to_string_lossy()
        .trim_start_matches("\\\\?\\")
        .to_string();
    // A reused benchmark already has its workspace and history links. The
    // indexer may have resolved its physical identity since the initial seed;
    // re-registering the seed spelling can collide with that canonical row.
    if !reuse {
        service.register_workspace(&Workspace {
            id: "bench-workspace".to_string(),
            path: HostPath {
                host_id: "local-windows".to_string(),
                display_path: workspace_dir.to_string_lossy().to_string(),
                canonical_path: Some(canonical_workspace),
            },
            repo_group_id: None,
            worktree_id: None,
        })
        .map_err(|error| error.to_string())?;
    }

    let source = tooling::register_source(&service, "codex", &history_root, "bench", true)
        .map_err(|error| error.to_string())?;
    let index_started = Instant::now();
    let mut passes = 0;
    let mut imported = 0_u64;
    let mut last_complete = false;
    let mut start_index = 0_usize;
    while passes < 200 {
        passes += 1;
        let report = tooling::run_scan(
            &service,
            &source,
            ScanOptions {
                budget_ms: 20_000,
                start_index,
                ..ScanOptions::incremental()
            },
        )
        .map_err(|error| error.to_string())?;
        start_index = report.next_index;
        imported += report.progress.messages;
        last_complete = report.complete;
        eprintln!(
            "pass {passes}: listed={} done={} skipped={} failed={} messages={} complete={}",
            report.listed_files,
            report.progress.files_done,
            report.progress.files_skipped,
            report.progress.files_failed,
            report.progress.messages,
            report.complete
        );
        if report.complete {
            break;
        }
    }
    let index_seconds = index_started.elapsed().as_secs_f64();
    let (stored, cursors, row_errors) =
        tooling::source_counts(&service, &source.id).map_err(|error| error.to_string())?;
    let generated_messages = if reuse {
        u64::try_from(stored).unwrap_or(0)
    } else {
        total_messages as u64
    };

    let mut list = Vec::with_capacity(SAMPLES);
    let mut messages = Vec::with_capacity(SAMPLES);
    let mut page_two = Vec::with_capacity(SAMPLES);
    // Last observed result shape, so the JSON proves the caps and the match
    // mode instead of leaving them to inference.
    let mut observed = serde_json::Map::new();

    // Resolved once: the message-page timing must cover the page query only.
    let bench_session_id = service
        .list_sessions(&SessionQuery {
            workspace_id: None,
            agent_id: None,
            origin: None,
            limit: 1,
            cursor: None,
        })
        .map_err(|error| error.to_string())?
        .items
        .first()
        .map(|item| item.session.id.clone())
        .ok_or("no session for the message page")?;

    // Every case matches every synthetic message, so the numbers are a worst
    // case rather than a lucky keyword. The three filters are measured
    // separately because the unfiltered optimisation does not cover them.
    let cases = search_cases(&source.id, "bench-workspace");
    let mut search_samples: Vec<Vec<Duration>> =
        vec![Vec::with_capacity(SAMPLES); cases.len()];

    // A real second page: the cursor comes from a first page, so the timing
    // includes the keyset predicates.
    let first_page = query::search_history(&service, &cases[0].query(50, None))
        .map_err(|error| error.to_string())?;
    let page_cursor = first_page
        .next_cursor
        .clone()
        .ok_or("the benchmark query has no second page")?;

    for iteration in 0..(SAMPLES + WARMUP) {
        let started = Instant::now();
        let page = service
            .list_sessions(&SessionQuery {
                workspace_id: None,
                agent_id: None,
                origin: None,
                limit: 50,
                cursor: None,
            })
            .map_err(|error| error.to_string())?;
        if iteration >= WARMUP {
            list.push(started.elapsed());
            if page.items.is_empty() {
                return Err("list returned no session".into());
            }
        }

        let started = Instant::now();
        let page = query::message_page(
            &service,
            &MessagePageQuery {
                session_id: bench_session_id.clone(),
                limit: 50,
                cursor: None,
                anchor_message_id: None,
            },
        )
        .map_err(|error| error.to_string())?;
        if iteration >= WARMUP {
            messages.push(started.elapsed());
        }
        if page.items.is_empty() {
            return Err("message page returned no row".into());
        }
        observed.insert(
            "messagePage".into(),
            json!({
                "total": page.total,
                "returned": page.items.len(),
            }),
        );

        let started = Instant::now();
        let page = query::search_history(
            &service,
            &cases[0].query(50, Some(page_cursor.clone())),
        )
        .map_err(|error| error.to_string())?;
        if iteration >= WARMUP {
            page_two.push(started.elapsed());
        }
        if page.items.is_empty() {
            return Err("second search page returned no row".into());
        }
        observed.insert(
            "searchCjkFtsPage2".into(),
            json!({
                "total": page.total,
                "returned": page.items.len(),
                "cursorReused": true,
            }),
        );

        for (index, case) in cases.iter().enumerate() {
            let started = Instant::now();
            let page = query::search_history(&service, &case.query(50, None))
                .map_err(|error| error.to_string())?;
            if iteration >= WARMUP {
                search_samples[index].push(started.elapsed());
            }
            if page.items.is_empty() {
                return Err(format!("{} returned no result", case.name));
            }
            observed.insert(
                case.name.into(),
                json!({
                    "total": page.total,
                    "returned": page.items.len(),
                    "matchMode": page.match_mode,
                    "filters": {
                        "agentId": case.agent,
                        "workspaceId": case.workspace,
                        "sourceId": case.source,
                    },
                }),
            );
        }
    }

    let mut results = serde_json::Map::new();
    results.insert("listSessions".into(), stats(&list));
    results.insert("messagePage".into(), stats(&messages));
    results.insert("searchCjkFtsPage2".into(), stats(&page_two));
    for (index, case) in cases.iter().enumerate() {
        results.insert(case.name.to_string(), stats(&search_samples[index]));
    }

    let db_bytes = std::fs::metadata(service.database_path())
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let output = json!({
        "generatedSessions": args.sessions,
        "generatedMessages": generated_messages,
        "indexedMessages": stored,
        "cursors": cursors,
        "rowErrors": row_errors,
        "generateSeconds": round(generate_seconds),
        "indexSeconds": round(index_seconds),
        "indexPasses": passes,
        "indexComplete": last_complete,
        "importedThisRun": imported,
        "databaseBytes": db_bytes,
        "machine": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cpu": std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".into()),
        },
        "observed": observed,
        "method": {
            "samples": SAMPLES,
            "warmup": WARMUP,
            "percentile": "nearest-rank over sorted samples: index = ceil(0.95*n)-1",
            "timingScope": "one synchronous service call (SQL + mapping); no IPC, no React",
            "resultCap": 50,
            "searchFallback": "history_message_fts.body LIKE with escaped wildcards for 1-2 character queries"
        },
        "results": results,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output).map_err(|error| error.to_string())?
    );
    Ok(())
}

struct SearchCase {
    name: &'static str,
    text: &'static str,
    agent: Option<&'static str>,
    workspace: Option<&'static str>,
    source: Option<&'static str>,
}

impl SearchCase {
    fn query(&self, limit: u32, cursor: Option<String>) -> HistorySearchQuery {
        HistorySearchQuery {
            text: self.text.to_string(),
            workspace_id: self.workspace.map(str::to_string),
            agent_id: self.agent.map(str::to_string),
            source_id: self.source.map(str::to_string),
            limit,
            cursor,
        }
    }
}

/// Four texts (Chinese FTS, Chinese short LIKE, English FTS, literal percent)
/// crossed with no filter and each of the three filters. `100%` is included
/// because a literal `%` must not become a wildcard.
fn search_cases(source_id: &str, workspace_id: &str) -> Vec<SearchCase> {
    let mut cases = Vec::new();
    for (text, label) in [
        ("历史记录", "CjkFts"),
        ("中文", "CjkShortLike"),
        ("recorder", "EnglishFts"),
    ] {
        cases.push(SearchCase {
            name: Box::leak(format!("search{label}Unfiltered").into_boxed_str()),
            text,
            agent: None,
            workspace: None,
            source: None,
        });
        cases.push(SearchCase {
            name: Box::leak(format!("search{label}ByAgent").into_boxed_str()),
            text,
            agent: Some("codex"),
            workspace: None,
            source: None,
        });
        cases.push(SearchCase {
            name: Box::leak(format!("search{label}ByWorkspace").into_boxed_str()),
            text,
            agent: None,
            workspace: Some(Box::leak(workspace_id.to_string().into_boxed_str())),
            source: None,
        });
        cases.push(SearchCase {
            name: Box::leak(format!("search{label}BySource").into_boxed_str()),
            text,
            agent: None,
            workspace: None,
            source: Some(Box::leak(source_id.to_string().into_boxed_str())),
        });
    }
    // A literal `%` must not become a wildcard; measured once, unfiltered.
    cases.push(SearchCase {
        name: "searchPercentLiteralUnfiltered",
        text: "100%",
        agent: None,
        workspace: None,
        source: None,
    });
    cases
}

fn generate(args: &Args, root: &PathBuf, workspace: &PathBuf) -> Result<usize, String> {
    let workspace_text = workspace.to_string_lossy().replace('\\', "\\\\");
    let mut count = 0_usize;
    for index in 0..args.sessions {
        let shard = root.join(format!("{:03}", index % 100));
        std::fs::create_dir_all(&shard).map_err(|error| error.to_string())?;
        let path = shard.join(format!("session-{index:05}.jsonl"));
        let session = format!("bench-{index:05}");
        let mut text = String::with_capacity(4096);
        text.push_str(&format!(
            "{{\"ordinal\":0,\"timestamp\":\"2026-11-02T00:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{{\"session_id\":\"{session}\",\"id\":\"{session}\",\"cwd\":\"{workspace_text}\",\"cli_version\":\"0.166.0\"}}}}\n"
        ));
        for turn in 0..args.messages {
            let role = if turn % 2 == 0 { "user" } else { "assistant" };
            let block = if role == "user" { "input_text" } else { "output_text" };
            text.push_str(&format!(
                "{{\"ordinal\":{},\"timestamp\":\"2026-11-02T00:{:02}:{:02}.000Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"id\":\"{session}-{turn:03}\",\"role\":\"{role}\",\"content\":[{{\"type\":\"{block}\",\"text\":\"基准记录 {session} 第 {turn} 条 中文历史记录 recorder 100% done\"}}]}}}}\n",
                turn + 1,
                (turn / 60) % 60,
                turn % 60
            ));
            count += 1;
        }
        std::fs::write(&path, text.as_bytes()).map_err(|error| error.to_string())?;
    }
    Ok(count)
}

fn stats(samples: &[Duration]) -> serde_json::Value {
    let mut millis: Vec<f64> = samples.iter().map(|value| value.as_secs_f64() * 1000.0).collect();
    millis.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let p50 = percentile(&millis, 0.50);
    let p95 = percentile(&millis, 0.95);
    let max = millis.last().copied().unwrap_or(0.0);
    json!({
        "samples": millis.len(),
        "p50Ms": round(p50),
        "p95Ms": round(p95),
        "maxMs": round(max),
    })
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[index]
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn parse_args() -> Result<Args, String> {
    let mut dir: Option<PathBuf> = None;
    let mut sessions = 10_000_usize;
    let mut messages = 10_usize;
    let mut reuse = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dir = Some(PathBuf::from(args.next().ok_or("--dir needs a value")?)),
            "--reuse" => reuse = true,
            "--sessions" => {
                sessions = args
                    .next()
                    .ok_or("--sessions needs a value")?
                    .parse()
                    .map_err(|_| "--sessions must be a number")?
            }
            "--messages" => {
                messages = args
                    .next()
                    .ok_or("--messages needs a value")?
                    .parse()
                    .map_err(|_| "--messages must be a number")?
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        dir: dir.ok_or("--dir is required")?,
        sessions,
        messages,
        reuse,
    })
}
