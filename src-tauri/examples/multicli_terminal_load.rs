//! S04 performance harness: 8 managed terminals × 100 KiB/s × 60 s.
//!
//! This drives the same `TerminalManager` the IPC layer uses, with the same
//! 4 MiB ring, the same 20 MiB log and the same 64 KiB read batches, but
//! without the WebView. What it measures and what it deliberately excludes is
//! printed in the JSON so the delivery report cannot overstate it:
//!
//! included: PTY → ConPTY → reader thread → bounded ring → `read` batch, and
//! the write path back into the PTY;
//! excluded: React rendering, xterm paint and the Tauri IPC serialisation.
//!
//! Every process is created by this program through the manager; nothing is
//! looked up by image name and nothing is killed outside an owned tree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ccgui_next_lib::multicli::terminal::{
    TerminalCloseRequest, TerminalManager, TerminalNotify, TerminalOpenSpec, TerminalReadRequest,
    TerminalStopMode, TerminalTarget, TerminalTargetKind,
};
use ccgui_next_lib::multicli::{Host, MultiCliService};
use serde_json::json;

struct Options {
    terminals: usize,
    kib_per_second: usize,
    seconds: u64,
    rounds: usize,
    start_round: usize,
    echo_samples: usize,
    out: PathBuf,
    root: PathBuf,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        terminals: 8,
        kib_per_second: 100,
        seconds: 60,
        rounds: 20,
        start_round: 0,
        echo_samples: 40,
        out: PathBuf::from("multicli-s04-load.json"),
        root: std::env::temp_dir().join("multicli-s04-load"),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("missing value for {arg}"));
        match arg.as_str() {
            "--terminals" => options.terminals = value()?.parse().map_err(|e| format!("{e}"))?,
            "--rate-kib" => options.kib_per_second = value()?.parse().map_err(|e| format!("{e}"))?,
            "--seconds" => options.seconds = value()?.parse().map_err(|e| format!("{e}"))?,
            "--rounds" => options.rounds = value()?.parse().map_err(|e| format!("{e}"))?,
            "--start-round" => options.start_round = value()?.parse().map_err(|e| format!("{e}"))?,
            "--echo-samples" => options.echo_samples = value()?.parse().map_err(|e| format!("{e}"))?,
            "--out" => options.out = PathBuf::from(value()?),
            "--root" => options.root = PathBuf::from(value()?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(options)
}

fn now_iso() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0);
    // The harness only needs a sortable stamp, not a full calendar conversion.
    format!("epoch-ms:{millis}")
}

fn write_flood_script(root: &Path, kib_per_second: usize, seconds: u64) -> Result<PathBuf, String> {
    let chunk = (kib_per_second * 1024 / 100).max(256);
    let path = root.join("flood.ps1");
    // Paced by elapsed time rather than "write then sleep": `Start-Sleep
    // -Milliseconds 10` actually sleeps ~15.6 ms, which would silently turn a
    // 100 KiB/s target into ~64 KiB/s. The loop writes whatever the elapsed
    // time says should already be written, so the average rate holds.
    let body = format!(
        "$ErrorActionPreference = 'Continue'\r\n\
         $chunk = 'x' * {chunk}\r\n\
         $rate = {rate}\r\n\
         $deadline = [DateTime]::UtcNow.AddSeconds({seconds})\r\n\
         $sw = [Diagnostics.Stopwatch]::StartNew()\r\n\
         $written = 0L\r\n\
         while ([DateTime]::UtcNow -lt $deadline) {{\r\n\
         \x20 $want = [long]($rate * $sw.Elapsed.TotalSeconds)\r\n\
         \x20 while ($written -lt $want) {{\r\n\
         \x20   [Console]::Out.Write($chunk)\r\n\
         \x20   $written += $chunk.Length\r\n\
         \x20 }}\r\n\
         \x20 Start-Sleep -Milliseconds 5\r\n\
         }}\r\n",
        rate = kib_per_second * 1024
    );
    std::fs::write(&path, body).map_err(|error| format!("write flood script: {error}"))?;
    Ok(path)
}

fn flood_command(script: &Path, pid_file: &Path, index: usize) -> String {
    // `start /b` returns to the prompt immediately, so the shell stays
    // interactive while a detached child writes to the same console — exactly
    // the situation a user creates with a long-running CLI.
    format!(
        "start /b powershell -NoProfile -ExecutionPolicy Bypass -Command \"$PID | Set-Content -Encoding ASCII '{pid}'; & '{script}'\"\r",
        pid = pid_file.display(),
        script = script.display()
    )
}

fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

#[cfg(windows)]
fn handle_count() -> Option<u32> {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    let mut count = 0u32;
    let ok = unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) };
    if ok.is_ok() {
        Some(count)
    } else {
        None
    }
}

#[cfg(not(windows))]
fn handle_count() -> Option<u32> {
    None
}

fn process_alive(pid: u32) -> bool {
    use ccgui_next_lib::multicli::discovery::winproc::{HostBackend, WindowsProcessBackend};
    WindowsProcessBackend::new("local-windows")
        .inspect(pid)
        .is_ok()
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

fn main() {
    if let Err(error) = run() {
        eprintln!("multicli_terminal_load: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = parse_options()?;
    std::fs::create_dir_all(&options.root).map_err(|error| format!("create root: {error}"))?;
    let cwd = options.root.join("work 中文 dir");
    std::fs::create_dir_all(&cwd).map_err(|error| format!("create cwd: {error}"))?;
    let flood_script = write_flood_script(&options.root, options.kib_per_second, options.seconds)?;

    let service = Arc::new(
        MultiCliService::open(options.root.join("profile")).map_err(|error| error.message)?,
    );
    service
        .register_host(&Host::local("local-windows"))
        .map_err(|error| error.message)?;
    let manager = Arc::new(
        TerminalManager::new(
            Arc::clone(&service),
            options.root.join("profile"),
            "test".into(),
            Arc::new(|_: TerminalNotify| {}) as ccgui_next_lib::multicli::terminal::NotifyFn,
        )
        .map_err(|error| error.message)?,
    );

    // A single process cannot run 20 × 60 s inside one tool call, so the harness
    // appends to the same report and records where each chunk started. The
    // handle curve therefore also shows the per-process baseline of every chunk.
    let mut previous_rounds: Vec<serde_json::Value> = Vec::new();
    let mut previous_samples: Vec<f64> = Vec::new();
    let mut previous_bytes = 0u64;
    let mut chunks: Vec<serde_json::Value> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(&options.out) {
        if let Ok(existing) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(items) = existing.get("roundsDetail").and_then(|value| value.as_array()) {
                previous_rounds = items.clone();
            }
            if let Some(items) = existing
                .get("echo")
                .and_then(|value| value.get("all"))
                .and_then(|value| value.as_array())
            {
                previous_samples = items
                    .iter()
                    .filter_map(|value| value.as_f64())
                    .collect();
            }
            previous_bytes = existing
                .get("totalBytes")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            if let Some(items) = existing.get("chunks").and_then(|value| value.as_array()) {
                chunks = items.clone();
            }
        }
    }

    let mut rounds = Vec::new();
    let mut echo_samples: Vec<f64> = Vec::new();
    let mut all_bytes = 0u64;
    let process_start_handles = handle_count();

    for round in options.start_round..(options.start_round + options.rounds) {
        let started = Instant::now();
        let round_sample_base = echo_samples.len();
        let handles_before = handle_count();
        let mut terminals = Vec::new();
        let mut pid_files = Vec::new();

        for index in 0..options.terminals {
            let spec = TerminalOpenSpec {
                target: TerminalTarget {
                    kind: TerminalTargetKind::Shell,
                    shell: Some(ccgui_next_lib::multicli::terminal::ShellFlavor::Cmd),
                    installation_id: None,
                },
                args: Vec::new(),
                cwd: cwd.display().to_string(),
                env: Vec::new(),
                cols: 120,
                rows: 40,
                title: Some(format!("load-{round}-{index}")),
            };
            let view = manager.open(&spec).map_err(|error| error.message)?;
            let pid_file = options.root.join(format!("flood-{round}-{index}.pid"));
            let _ = std::fs::remove_file(&pid_file);
            manager
                .write(&ccgui_next_lib::multicli::terminal::TerminalWriteRequest {
                    terminal_id: view.terminal_id.clone(),
                    data: flood_command(&flood_script, &pid_file, index),
                })
                .map_err(|error| error.message)?;
            terminals.push(view);
            pid_files.push(pid_file);
        }

        // Wait until every flooder reported its pid, then start measuring.
        let pid_deadline = Instant::now() + Duration::from_secs(20);
        let mut flood_pids: Vec<u32> = Vec::new();
        while flood_pids.len() < terminals.len() && Instant::now() < pid_deadline {
            flood_pids.clear();
            for file in &pid_files {
                if let Some(pid) = read_pid(file) {
                    flood_pids.push(pid);
                }
            }
            if flood_pids.len() < terminals.len() {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        if flood_pids.len() != terminals.len() {
            return Err(format!(
                "only {} of {} flooders reported a pid",
                flood_pids.len(),
                terminals.len()
            ));
        }

        let run_started = Instant::now();
        let mut cursors: BTreeMap<String, u64> = terminals
            .iter()
            .map(|view| (view.terminal_id.clone(), 0u64))
            .collect();
        let mut round_bytes = 0u64;
        let mut echo_sent: Vec<(String, Instant)> = Vec::new();
        let mut next_echo = Instant::now();
        // Samples are spread over every round, not burned in the first one, so
        // the p95 covers the whole 20-round session and not a single warm round.
        let mut round_echo_sent = 0usize;
        let active_id = terminals[0].terminal_id.clone();
        let mut echo_index = 0usize;

        while run_started.elapsed() < Duration::from_secs(options.seconds) {
            if round_echo_sent < options.echo_samples && Instant::now() >= next_echo {
                echo_index += 1;
                let marker = format!("PING-{round}-{echo_index}");
                manager
                    .write(&ccgui_next_lib::multicli::terminal::TerminalWriteRequest {
                        terminal_id: active_id.clone(),
                        data: format!("echo {marker}\r"),
                    })
                    .map_err(|error| error.message)?;
                echo_sent.push((marker, Instant::now()));
                round_echo_sent += 1;
                next_echo = Instant::now() + Duration::from_millis(120);
            }
            for view in &terminals {
                let cursor = *cursors.get(&view.terminal_id).unwrap_or(&0);
                let read = manager
                    .read(&TerminalReadRequest {
                        terminal_id: view.terminal_id.clone(),
                        cursor: Some(cursor),
                        max_bytes: Some(256 * 1024),
                    })
                    .map_err(|error| error.message)?;
                cursors.insert(view.terminal_id.clone(), read.to_seq);
                round_bytes += read.bytes;
                if !read.text.is_empty() {
                    for (marker, sent_at) in echo_sent.clone() {
                        if read.text.contains(&marker) {
                            echo_samples.push(sent_at.elapsed().as_secs_f64() * 1000.0);
                            echo_sent.retain(|(candidate, _)| candidate != &marker);
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        all_bytes += round_bytes;

        // The active-terminal echo must be observed within the load window; a
        // missing sample is recorded as a miss, never filled in.
        let mut missed = 0usize;
        for _ in echo_sent {
            missed += 1;
        }

        let mut stopped = Vec::new();
        for view in &terminals {
            let closed = manager
                .close(&TerminalCloseRequest {
                    terminal_id: view.terminal_id.clone(),
                    mode: Some(TerminalStopMode::Force),
                    grace_ms: Some(2_000),
                })
                .map_err(|error| error.message)?;
            stopped.push(closed);
        }

        // Orphan check: every flooder was a grandchild of a terminal; after the
        // close none of them may still be alive.
        std::thread::sleep(Duration::from_millis(600));
        let survivors: Vec<u32> = flood_pids
            .iter()
            .copied()
            .filter(|pid| process_alive(*pid))
            .collect();
        let residual = manager.list().active_count;
        let handles_after = handle_count();

        rounds.push(json!({
            "round": round,
            "seconds": options.seconds,
            "terminals": terminals.len(),
            "bytes": round_bytes,
            "elapsedMs": started.elapsed().as_millis(),
            "echoSamples": echo_samples.len() - round_sample_base,
            "echoMissed": missed,
            "handlesBefore": handles_before,
            "handlesAfter": handles_after,
            "flooderSurvivors": survivors,
            "activeTerminalsAfterClose": residual,
            "closeReports": stopped.iter().map(|item| json!({
                "removed": item.removed,
                "escalated": item.stop.escalated,
                "alreadyTerminal": item.stop.already_terminal,
            })).collect::<Vec<_>>(),
        }));

        for file in &pid_files {
            let _ = std::fs::remove_file(file);
        }
    }

    chunks.push(json!({
        "startRound": options.start_round,
        "rounds": options.rounds,
        "processStartHandles": process_start_handles,
        "roundsCompleted": rounds.len(),
    }));
    previous_rounds.extend(rounds);
    previous_samples.extend(echo_samples);
    let mut sorted = previous_samples;
    sorted.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let summary = json!({
        "generatedAt": now_iso(),
        "terminals": options.terminals,
        "kibPerSecondEach": options.kib_per_second,
        "secondsPerRound": options.seconds,
        "rounds": previous_rounds.len(),
        "totalBytes": previous_bytes + all_bytes,
        "chunks": chunks,
        "echo": {
            "samples": sorted.len(),
            "p50Ms": percentile(&sorted, 0.50),
            "p95Ms": percentile(&sorted, 0.95),
            "maxMs": sorted.last().copied().unwrap_or(0.0),
            "all": sorted,
        },
        "measurementScope": "PTY/ConPTY -> reader thread -> 4 MiB ring -> 64-256 KiB read batch; excludes React/xterm paint and Tauri IPC serialisation",
        "limits": manager.limits(),
        "roundsDetail": previous_rounds,
    });
    let text = serde_json::to_string_pretty(&summary).map_err(|error| error.to_string())?;
    std::fs::write(&options.out, text).map_err(|error| format!("write {}: {error}", options.out.display()))?;
    println!("{}", serde_json::to_string(&json!({
        "out": options.out.display().to_string(),
        "p95Ms": percentile(&sorted, 0.95),
        "samples": sorted.len(),
    })).unwrap());
    manager.hard_shutdown_all();
    Ok(())
}
