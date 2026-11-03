import { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { Terminal } from "@xterm/xterm";

import TerminalPane from "../../src/multicli/terminal/TerminalPane";
import "../../src/lib/i18n";
import "../../src/i18n/multicli";
import type { MultiCliApi } from "../../src/multicli/api";
import type {
  AppError,
  TerminalReadQuery,
  TerminalReadView,
  TerminalResizeView,
  TerminalRunState,
  TerminalWriteView,
} from "../../src/multicli/types";

type Started = {
  round: number;
  terminals: Array<{
    terminalId: string;
    title: string;
    pid: number | null;
    creationTime: string | null;
    openedAtEpochMs: number;
  }>;
  handlesBefore: number | null;
  activeTerminalsBefore: number;
  activeTerminalsAfterOpen: number;
};

type Sample = { marker: string; terminalId: string; latencyMs: number; invokeLatencyMs: number; dispatchDelayMs: number };
type PendingSample = {
  marker: string;
  terminalId: string;
  startedAt: number;
  invokedAt: number;
  resolve: (sample: Sample | null) => void;
  timeout: number;
};

const paneTerminals = new WeakMap<Terminal, string>();
const openPaneIds = new Set<string>();
const firstReadIds = new Set<string>();
const readBytes = new Map<string, number>();
const readCalls = new Map<string, number>();
const droppedBytes = new Map<string, number>();
const scanTail = new Map<string, string>();
const completedFloods = new Map<string, { bytes: number; elapsedMs: number }>();
const pendingSamples = new Map<string, PendingSample>();
const frameScheduled = new Set<string>();
const failures: Array<{ at: number; terminalId?: string; message: string }> = [];
const allSamples: Sample[] = [];
const waiters: Array<() => void> = [];

function signalWaiters() {
  for (const notify of waiters.splice(0)) notify();
}

function markFailure(message: string, terminalId?: string) {
  failures.push({ at: performance.now(), terminalId, message });
}

const originalOpen = Terminal.prototype.open;
Terminal.prototype.open = function (this: Terminal, parent: HTMLElement) {
  const owner = parent.closest<HTMLElement>("[data-testid^='terminal-pane-']")
    ?.dataset.testid?.slice("terminal-pane-".length);
  if (owner) {
    paneTerminals.set(this, owner);
    openPaneIds.add(owner);
    signalWaiters();
  }
  return originalOpen.call(this, parent);
};

const originalWrite = Terminal.prototype.write;
Terminal.prototype.write = function (
  this: Terminal,
  data: string | Uint8Array,
  callback?: () => void,
) {
  const terminalId = paneTerminals.get(this);
  const text = typeof data === "string" ? data : new TextDecoder().decode(data);
  const alreadyScheduled: string[] = [];
  let completed: RegExpMatchArray | null = null;
  if (terminalId) {
    const previous = scanTail.get(terminalId) ?? "";
    const combined = previous + text;
    completed = combined.match(/BENCH_DONE:(\d+):(\d+)[\r\n]/);
    for (const [marker, pending] of pendingSamples) {
      if (
        pending.terminalId === terminalId &&
        !frameScheduled.has(marker) &&
        combined.includes(`BENCH_ACK:${marker}`)
      ) {
        frameScheduled.add(marker);
        alreadyScheduled.push(marker);
      }
    }
    scanTail.set(terminalId, combined.slice(-512));
  }

  return originalWrite.call(this, data, () => {
    callback?.();
    if (terminalId && completed) {
      completedFloods.set(terminalId, { bytes: Number(completed[1]), elapsedMs: Number(completed[2]) });
      signalWaiters();
    }
    if (alreadyScheduled.length === 0) return;
    requestAnimationFrame(() => requestAnimationFrame(() => {
      for (const marker of alreadyScheduled) {
        const pending = pendingSamples.get(marker);
        if (!pending) continue;
        window.clearTimeout(pending.timeout);
        pendingSamples.delete(marker);
        frameScheduled.delete(marker);
        const sample = {
          marker,
          terminalId: pending.terminalId,
          latencyMs: performance.now() - pending.startedAt,
          invokeLatencyMs: performance.now() - pending.invokedAt,
          dispatchDelayMs: pending.invokedAt - pending.startedAt,
        };
        allSamples.push(sample);
        pending.resolve(sample);
      }
    }));
  });
};

function waitFor(predicate: () => boolean, timeoutMs: number, label: string): Promise<void> {
  if (predicate()) return Promise.resolve();
  return new Promise((resolve, reject) => {
    let done = false;
    const timeout = window.setTimeout(() => {
      if (done) return;
      done = true;
      reject(new Error(`timeout waiting for ${label}`));
    }, timeoutMs);
    const check = () => {
      if (done) return;
      if (!predicate()) {
        waiters.push(check);
        return;
      }
      done = true;
      window.clearTimeout(timeout);
      resolve();
    };
    check();
  });
}

function delay(ms: number) {
  return new Promise<void>((resolve) => window.setTimeout(resolve, ms));
}

function percentile(values: number[], fraction: number) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.round((sorted.length - 1) * fraction)];
}

function attachCloseCheckFailures(report: Record<string, unknown>) {
  const problems: string[] = [];
  if (report.unfinalizedRunsAfterClose !== 0) problems.push(`unfinalized durable Runs after close: ${String(report.unfinalizedRunsAfterClose)}`);
  if (report.activeTerminalsAfterClose !== 0) {
    problems.push(`active terminal count after close was ${String(report.activeTerminalsAfterClose)}`);
  }
  const checks = Array.isArray(report.syntheticProcessChecks)
    ? report.syntheticProcessChecks as Array<Record<string, unknown>>
    : [];
  for (const check of checks) {
    if (check.status !== "gone" && check.status !== "original_gone_pid_reused" && !String(check.status).startsWith("exited_with_code_")) {
      problems.push(`synthetic pid ${String(check.pid)} check was ${String(check.status)}`);
    }
  }
  const existing = Array.isArray(report.failures) ? report.failures : [];
  report.failures = [
    ...existing,
    ...problems.map((message) => ({ at: performance.now(), message })),
  ];
}

function configFromUrl() {
  const query = new URLSearchParams(window.location.search);
  const rounds = Number(query.get("rounds") ?? "20");
  const seconds = Number(query.get("seconds") ?? "60");
  const samples = Number(query.get("samples") ?? "12");
  const autoStart = query.get("autostart") !== "0";
  if (!Number.isInteger(rounds) || rounds < 1 || rounds > 20) throw new Error("rounds must be 1..20");
  if (!Number.isInteger(seconds) || !((seconds >= 1 && seconds <= 20) || seconds === 60)) {
    throw new Error("seconds must be 1..20 for smoke or exactly 60 for the full run");
  }
  if (!Number.isInteger(samples) || samples < 4 || samples > 40) throw new Error("samples must be 4..40");
  return { rounds, seconds, samples, autoStart };
}

function BenchApp() {
  const [config] = useState(configFromUrl);
  const [terminals, setTerminals] = useState<Started["terminals"]>([]);
  const [status, setStatus] = useState("Ready. This uses synthetic processes and a dedicated scratch profile.");
  const [running, setRunning] = useState(false);
  const [savedPath, setSavedPath] = useState("");
  const [roundSummary, setRoundSummary] = useState<Array<Record<string, unknown>>>([]);
  const autoStarted = useRef(false);

  const api = useMemo(() => {
    const terminalRead = async (query: TerminalReadQuery): Promise<TerminalReadView> => {
      readCalls.set(query.terminalId, (readCalls.get(query.terminalId) ?? 0) + 1);
      const view = await invoke<TerminalReadView>("multicli_terminal_read", { query });
      readBytes.set(query.terminalId, (readBytes.get(query.terminalId) ?? 0) + view.bytes);
      firstReadIds.add(query.terminalId);
      signalWaiters();
      return view;
    };
    const terminalWrite = async (terminalId: string, data: string): Promise<TerminalWriteView> =>
      invoke<TerminalWriteView>("multicli_terminal_write", { query: { terminalId, data } });
    const terminalResize = async (
      terminalId: string,
      cols: number,
      rows: number,
    ): Promise<TerminalResizeView> =>
      invoke<TerminalResizeView>("multicli_terminal_resize", { query: { terminalId, cols, rows } });
    return { terminalRead, terminalWrite, terminalResize } as unknown as MultiCliApi;
  }, []);

  const recordError = (error: AppError, terminalId: string) => {
    markFailure(`${error.code}: ${error.message}`, terminalId);
  };

  const run = async () => {
    if (running) return;
    setRunning(true);
    setSavedPath("");
    setRoundSummary([]);
    try {
      for (let round = 0; round < config.rounds; round += 1) {
        openPaneIds.clear();
        firstReadIds.clear();
        readBytes.clear();
        readCalls.clear();
        droppedBytes.clear();
        scanTail.clear();
        completedFloods.clear();
        failures.length = 0;
        allSamples.length = 0;
        pendingSamples.clear();
        frameScheduled.clear();

        setStatus(`Round ${round + 1}/${config.rounds}: opening eight managed terminals…`);
        const started = await invoke<Started>("bench_start", {
          round,
          seconds: config.seconds,
          rateKib: 100,
        });
        setTerminals(started.terminals);
        try {
          const ids = started.terminals.map((terminal) => terminal.terminalId);
          await waitFor(
            () => ids.every((id) => openPaneIds.has(id) && firstReadIds.has(id)),
            30_000,
            "all production TerminalPane instances and initial IPC reads",
          );

          const floodStartedAtEpochMs = Date.now() + 2_000;
          const runStartedAt = performance.now() + 2_000;
          await Promise.all(started.terminals.map((terminal) =>
            api.terminalWrite(
              terminal.terminalId,
              `START:${floodStartedAtEpochMs}\r\n`,
            ),
          ));
          await delay(Math.max(0, runStartedAt - performance.now()));
          const frontendStartLagMs = performance.now() - runStartedAt;
          const pendingResults: Promise<Sample | null>[] = [];
          for (let sampleIndex = 0; sampleIndex < config.samples; sampleIndex += 1) {
            const at = runStartedAt + ((sampleIndex + 1) * config.seconds * 1000) / (config.samples + 1);
            await delay(Math.max(0, at - performance.now()));
            const terminal = started.terminals[0];
            const marker = `UIBENCH-${round}-${sampleIndex}-${Math.floor(performance.timeOrigin)}`;
            let resolve!: (sample: Sample | null) => void;
            const result = new Promise<Sample | null>((done) => { resolve = done; });
            const timeout = window.setTimeout(() => {
              const pending = pendingSamples.get(marker);
              if (!pending) return;
              pendingSamples.delete(marker);
              frameScheduled.delete(marker);
              markFailure(`ACK timeout for ${marker}`, terminal.terminalId);
              pending.resolve(null);
            }, 12_000);
            pendingSamples.set(marker, {
              marker,
              terminalId: terminal.terminalId,
              startedAt: at,
              invokedAt: performance.now(),
              resolve,
              timeout,
            });
            try {
              await api.terminalWrite(terminal.terminalId, `PING:${marker}\r\n`);
            } catch (error) {
              window.clearTimeout(timeout);
              pendingSamples.delete(marker);
              markFailure(`terminalWrite failed: ${String(error)}`, terminal.terminalId);
              resolve(null);
            }
            pendingResults.push(result);
          }
          await delay(Math.max(0, runStartedAt + config.seconds * 1000 - performance.now()));
          await Promise.all(pendingResults);
          await waitFor(() => ids.every(id => completedFloods.has(id)), 15_000, "all eight complete flood markers parsed by xterm");
          const elapsedMs = performance.now() - runStartedAt;
          setStatus(`Round ${round + 1}/${config.rounds}: closing owned terminals and writing metrics…`);

          const latencies = allSamples.map((sample) => sample.latencyMs);
          const perTerminal = started.terminals.map((terminal) => {
            const terminalSamples = allSamples.filter((sample) => sample.terminalId === terminal.terminalId);
            const bytes = readBytes.get(terminal.terminalId) ?? 0;
            const dropped = droppedBytes.get(terminal.terminalId) ?? 0;
            return {
              terminalId: terminal.terminalId,
              pid: terminal.pid,
              creationTime: terminal.creationTime,
              openedAtEpochMs: terminal.openedAtEpochMs,
              floodStartedAtEpochMs,
              bytes,
              readCalls: readCalls.get(terminal.terminalId) ?? 0,
              elapsedMs: config.seconds * 1000,
              kibPerSecond: bytes / 1024 / config.seconds,
              droppedBytes: dropped,
              sourceFlood: completedFloods.get(terminal.terminalId),
              samples: terminalSamples.length,
              p50Ms: percentile(terminalSamples.map((sample) => sample.latencyMs), 0.5),
              p95Ms: percentile(terminalSamples.map((sample) => sample.latencyMs), 0.95),
            };
          });
          const roundReport = {
            round,
            requestedSeconds: config.seconds,
            elapsedMs,
            frontendStartLagMs,
            flooders: started.terminals.length,
            activeTerminalsBefore: started.activeTerminalsBefore,
            activeTerminalsAfterOpen: started.activeTerminalsAfterOpen,
            handlesBefore: started.handlesBefore,
            bytes: perTerminal.reduce((sum, terminal) => sum + terminal.bytes, 0),
            perTerminal,
            samples: allSamples,
            sampleCount: allSamples.length,
            p50Ms: percentile(latencies, 0.5),
            p95Ms: percentile(latencies, 0.95),
            sampleMisses: config.samples - allSamples.length,
            failures: [...failures],
          };
          setTerminals([]);
          await delay(250);
          const closed = await invoke<Record<string, unknown>>("bench_close", { report: roundReport });
          attachCloseCheckFailures(closed);
          const saved = await invoke<{ path: string }>("bench_report", { report: closed });
          setSavedPath(saved.path);
          setRoundSummary((previous) => [...previous, closed]);
        } catch (error) {
          markFailure(`round harness failed: ${String(error)}`);
          setTerminals([]);
          await delay(250);
          const failed = await invoke<Record<string, unknown>>("bench_close", {
            report: {
              round,
              requestedSeconds: config.seconds,
              failures: [...failures],
              sampleCount: allSamples.length,
              samples: allSamples,
            },
          });
          attachCloseCheckFailures(failed);
          const saved = await invoke<{ path: string }>("bench_report", { report: failed });
          setSavedPath(saved.path);
          setRoundSummary((previous) => [...previous, failed]);
          setStatus(`Stopped after round ${round + 1}: ${String(error)}`);
          return;
        }
      }
      setStatus(`Completed ${config.rounds} round(s). Report saved under the scratch benchmark root.`);
    } catch (error) {
      setStatus(`Stopped with a recorded failure: ${String(error)}`);
      try {
        const saved = await invoke<{ path: string }>("bench_report", {
          report: {
            round: roundSummary.length,
            failures: [{ at: performance.now(), message: String(error) }],
            sampleCount: allSamples.length,
            samples: allSamples,
          },
        });
        setSavedPath(saved.path);
      } catch {
        // If the Tauri command itself failed before startup, keep the raw error visible.
      }
    } finally {
      setTerminals([]);
      setRunning(false);
      if (config.autoStart) await invoke("bench_finish");
    }
  };

  useEffect(() => {
    if (!config.autoStart || autoStarted.current) return;
    autoStarted.current = true;
    void run();
  }, [config.autoStart]);

  return (
    <main className="app">
      <header>
        <div>
          <h1>MultiCLI S04 terminal UI benchmark</h1>
          <p>
            {config.rounds} round(s) × {config.seconds}s, 8 managed ConPTY terminals,
            100 KiB/s each, {config.samples} ACK samples per round.
          </p>
          <p className="scope">
            Input latency is scheduled input deadline → dispatch → helper ACK → production xterm parse callback → two RAFs.
            It excludes OS keyboard injection and does not claim physical display scanout.
          </p>
        </div>
        <button type="button" disabled={running} onClick={() => void run()}>
          {running ? "Benchmark running…" : "Start benchmark"}
        </button>
      </header>
      <p className="status" aria-live="polite">{status}</p>
      {savedPath && <p className="path">Report: {savedPath}</p>}
      <section className="terminal-grid" aria-label="Real production terminal panes">
        {terminals.map((terminal, index) => (
          <article
            className={`terminal-slot ${index === 0 ? "visible" : "hidden"}`}
            key={terminal.terminalId}
          >
            <h2>{terminal.title} · pid {terminal.pid ?? "pending"}</h2>
              <TerminalPane
              api={api}
              terminalId={terminal.terminalId}
              active={index === 0}
              onState={(_state: TerminalRunState, _exit) => {}}
              onDropped={(before, total) => {
                droppedBytes.set(terminal.terminalId, Math.max(before, total));
              }}
              onError={(error) => recordError(error, terminal.terminalId)}
              onCopyResult={() => {}}
            />
          </article>
        ))}
      </section>
      <section className="summary">
        <h2>Round summaries</h2>
        <pre>{JSON.stringify(roundSummary.map((round) => ({
          round: round.round,
          bytes: round.bytes,
          p95Ms: round.p95Ms,
          sampleCount: round.sampleCount,
          activeTerminalsAfterClose: round.activeTerminalsAfterClose,
          handlesBefore: round.handlesBefore,
          handlesAfter: round.handlesAfter,
          failureCount: Array.isArray(round.failures) ? round.failures.length : 0,
        })), null, 2)}</pre>
      </section>
    </main>
  );
}

const style = document.createElement("style");
style.textContent = `
  * { box-sizing: border-box; }
  html, body, #root { margin: 0; width: 100%; height: 100%; overflow: hidden; font: 13px Segoe UI, sans-serif; color: #17202a; background: #f5f7fa; }
  .app { height: 100%; display: grid; grid-template-rows: auto auto minmax(0, 1fr) 170px; gap: 8px; padding: 12px; }
  header { display:flex; justify-content:space-between; align-items:center; gap:20px; }
  h1 { margin:0 0 4px; font-size:18px; } p { margin:3px 0; } .scope { color:#596579; }
  button { padding:10px 18px; font-weight:600; } button:disabled { opacity:.6; }
  .status,.path { margin:0; padding:6px 10px; background:#fff; border:1px solid #d9e0e8; }
  .path { overflow-wrap:anywhere; }
  .terminal-grid { position:relative; min-height:0; display:grid; grid-template-columns:repeat(4,minmax(0,1fr)); grid-template-rows:repeat(2,minmax(0,1fr)); gap:6px; }
  .terminal-slot { min-width:0; min-height:0; overflow:hidden; display:flex; flex-direction:column; border:1px solid #bbc6d2; background:#fff; }
  .terminal-slot h2 { flex:none; margin:0; height:22px; padding:3px 6px; font:11px Segoe UI,sans-serif; }
  .terminal-slot > [data-testid^='terminal-pane-'] { position:relative; flex:1; min-height:0; }
  .terminal-slot > [data-testid^='terminal-pane-'] > .absolute.inset-0 { position:absolute; inset:0; }
  .terminal-slot > [data-testid^='terminal-pane-'] > .pointer-events-none { pointer-events:none; position:absolute; right:8px; bottom:4px; font-size:10px; color:#778394; }
  .terminal-slot.hidden { visibility:hidden; }
  .summary { min-height:0; overflow:auto; border:1px solid #d9e0e8; background:#fff; padding:4px 8px; }
  .summary h2 { margin:0 0 4px; font-size:12px; } pre { margin:0; white-space:pre-wrap; font-size:11px; }
`;
document.head.append(style);
createRoot(document.getElementById("root")!).render(<BenchApp />);
