import { useCallback, useEffect, useRef, useState } from "react";
import type { IDisposable, Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";
import type { WebglAddon } from "@xterm/addon-webgl";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";

import type { MultiCliApi } from "../api";
import type { AppError, TerminalExitView, TerminalNotify, TerminalRunState } from "../types";
import { toAppError } from "../navigation";
import { loadXterm } from "./xtermLoader";

/** One PTY read batch. The backend caps a reply at 256 KiB. */
const READ_BATCH = 64 * 1024;
/** Coalesce tiny writes before crossing IPC; 64 KiB/32 ms still drains 2 MiB/s. */
const MIN_PULL_INTERVAL_MS = 32;
/** Poll interval while the terminal runs; the event below usually beats it. */
const POLL_RUNNING_MS = 120;
/** A terminal that already ended only needs an occasional look. */
const POLL_ENDED_MS = 1_000;
const TERMINAL_EVENT = "multicli://terminal";
/** Bound queued plus in-flight UTF-8 input to the backend writer's limit. */
const INPUT_QUEUE_LIMIT = 1024 * 1024;

const LIGHT_THEME = {
  background: "#ffffff",
  foreground: "#1f2328",
  cursor: "#1f2328",
  cursorAccent: "#ffffff",
  selectionBackground: "#b6dcfe",
  black: "#24292f",
  red: "#cf222e",
  green: "#116329",
  yellow: "#4d2d00",
  blue: "#0969da",
  magenta: "#8250df",
  cyan: "#1b7c83",
  white: "#6e7781",
  brightBlack: "#57606a",
  brightRed: "#a40e26",
  brightGreen: "#1a7f37",
  brightYellow: "#633c01",
  brightBlue: "#218bff",
  brightMagenta: "#a475f9",
  brightCyan: "#3192aa",
  brightWhite: "#8c959f",
};
const DARK_THEME = {
  ...LIGHT_THEME,
  background: "#191919", foreground: "#e5e5e5", cursor: "#e5e5e5", cursorAccent: "#191919",
  selectionBackground: "#414141", black: "#272727", red: "#ff8a80", green: "#97d895",
  yellow: "#edce85", blue: "#8ab4f8", magenta: "#d6afff", cyan: "#86d8df", white: "#d4d4d4",
  brightBlack: "#8e8e8e", brightRed: "#ffb4a9", brightGreen: "#b8ebb2", brightYellow: "#ffe5a1",
  brightBlue: "#b0ccff", brightMagenta: "#e7c9ff", brightCyan: "#b5f0f3", brightWhite: "#ffffff",
};


async function copyText(text: string): Promise<boolean> {
  if (!text) return false;
  try {
    if (typeof navigator !== "undefined" && navigator.clipboard) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Fall through to the selection-based fallback below.
  }
  try {
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    const ok = document.execCommand?.("copy") ?? false;
    document.body.removeChild(area);
    return ok;
  } catch {
    return false;
  }
}

export interface TerminalPaneProps {
  api: MultiCliApi;
  terminalId: string;
  active: boolean;
  dark?: boolean;
  fontSize?: number;
  onState: (state: TerminalRunState, exit: TerminalExitView | null) => void;
  onDropped: (droppedBefore: number, totalDropped: number) => void;
  onError: (error: AppError) => void;
  onCopyResult: (copied: boolean) => void;
}

/**
 * One xterm instance for one backend terminal.
 *
 * The pane never decides anything about the process: it renders the byte
 * stream, forwards typed input as opaque data and reports what the backend
 * says. Reads are pull-based and only scheduled after xterm accepted the
 * previous batch (`term.write(..., callback)`), so a slow renderer exercises
 * the backend's bounded ring instead of growing an IPC queue.
 */
export default function TerminalPane({
  api,
  terminalId,
  active,
  dark = false,
  fontSize = 13,
  onState,
  onDropped,
  onError,
  onCopyResult,
}: TerminalPaneProps) {
  const { t } = useTranslation("multicli");
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const cursorRef = useRef<number | null>(null);
  const disposedRef = useRef(false);
  const inFlightRef = useRef(false);
  const wakeRequestedRef = useRef(false);
  const pollTimerRef = useRef<number | null>(null);
  const lastPullAtRef = useRef(Number.NEGATIVE_INFINITY);
  const lifecycleRef = useRef(0);
  const schedulePullRef = useRef<((delay: number) => void) | null>(null);
  const geometryRef = useRef({ width: 0, height: 0 });
  const lastStateRef = useRef<{ state: TerminalRunState; exitCode: number | null } | null>(null);
  const callbacksRef = useRef({ onState, onDropped, onError, onCopyResult });
  const syncGeometryRef = useRef<(() => void) | null>(null);
  const appearanceRef = useRef({ dark, fontSize });
  appearanceRef.current = { dark, fontSize };
  const [ready, setReady] = useState(false);

  // Parent callbacks are re-created on every render; keeping them in a ref is
  // what makes the read loop stable. Without this, each workspace re-render
  // would start a second loop and the terminal would be polled twice.
  useEffect(() => {
    callbacksRef.current = { onState, onDropped, onError, onCopyResult };
  }, [onState, onDropped, onError, onCopyResult]);

  useEffect(() => {
    disposedRef.current = false;
    const lifecycle = ++lifecycleRef.current;
    lastPullAtRef.current = Number.NEGATIVE_INFINITY;
    const isCurrent = () => !disposedRef.current && lifecycleRef.current === lifecycle;
    let term: Terminal | null = null;
    let fit: FitAddon | null = null;
    let webgl: WebglAddon | null = null;
    let input: IDisposable | null = null;
    const inputQueue: Array<{ data: string; bytes: number }> = [];
    let queuedInputBytes = 0;
    let drainingInput = false;
    let keydown: ((event: KeyboardEvent) => void) | null = null;
    let mounted = true;

    const fitNow = () => {
      try {
        fit?.fit();
      } catch {
        // Zero-size host during a layout pass; the read loop retries.
      }
    };

    /// Fits and pushes the geometry to the PTY, but only when the pane's layout
    /// box actually changed. Driven from the read loop rather than a
    /// ResizeObserver: the loop is the one code path that is guaranteed to run,
    /// and a hidden pane (no layout box) is skipped instead of fitting to zero.
    const syncGeometry = () => {
      const host = hostRef.current;
      const current = termRef.current;
      if (!host || !current || disposedRef.current) return;
      const width = host.clientWidth;
      const height = host.clientHeight;
      if (width === 0 || height === 0) return;
      const previous = geometryRef.current;
      if (previous.width === width && previous.height === height) return;
      fitNow();
      // xterm's fit addon can report a degenerate size before it has measured
      // the font; the backend rejects cols/rows below 2, so caching that size
      // would leave the PTY out of step until the next layout change.
      if (current.cols < 2 || current.rows < 2) return;
      geometryRef.current = { width, height };
      void api.terminalResize(terminalId, current.cols, current.rows).catch(() => {
        // A resize on an ended console is not actionable; the state read
        // reports the real status.
      });
    };

    void loadXterm()
      .then((modules) => {
        const host = hostRef.current;
        if (!mounted || !host) return;
        term = new modules.Terminal({
          fontFamily:
            '"JetBrains Mono", "Cascadia Mono", Consolas, "Microsoft YaHei Mono", monospace',
          fontSize: appearanceRef.current.fontSize,
          lineHeight: 1.15,
          cursorBlink: true,
          scrollback: 5000,
          theme: appearanceRef.current.dark ? DARK_THEME : LIGHT_THEME,
          convertEol: false,
          allowProposedApi: false,
        });
        termRef.current = term;
        fit = new modules.FitAddon();
        fitRef.current = fit;
        term.loadAddon(fit);
        term.open(host);
        try {
          // GPU renderer; xterm keeps the canvas renderer when WebGL is
          // unavailable (headless, blocklisted driver, lost context).
          webgl = new modules.WebglAddon();
          webgl.onContextLoss(() => {
            webgl?.dispose();
            webgl = null;
          });
          term.loadAddon(webgl);
        } catch {
          webgl = null;
        }
        fitNow();
        setReady(true);

        input = term.onData((data) => {
          const bytes = new TextEncoder().encode(data).byteLength;
          if (queuedInputBytes + bytes > INPUT_QUEUE_LIMIT) {
            callbacksRef.current.onError({
              code: "protocol",
              message: t("terminalInputQueueFull"),
              retryable: false,
              details: { limitBytes: INPUT_QUEUE_LIMIT, rejectedBytes: bytes },
            });
            return;
          }
          inputQueue.push({ data, bytes });
          queuedInputBytes += bytes;
          if (drainingInput) return;
          drainingInput = true;
          void (async () => {
            while (inputQueue.length > 0 && isCurrent()) {
              const item = inputQueue[0];
              try {
                await api.terminalWrite(terminalId, item.data);
              } catch (raw: unknown) {
                if (isCurrent()) callbacksRef.current.onError(toAppError(raw));
              } finally {
                inputQueue.shift();
                queuedInputBytes -= item.bytes;
              }
            }
            drainingInput = false;
          })();
        });

        // Ctrl+Shift+C copies the selection. Ctrl+C must stay the terminal's
        // interrupt, so the copy chord is explicitly shifted.
        keydown = (event: KeyboardEvent) => {
          if (event.ctrlKey && event.shiftKey && (event.key === "C" || event.key === "c")) {
            const selected = termRef.current?.getSelection() ?? "";
            if (!selected) return;
            event.preventDefault();
            event.stopPropagation();
            void copyText(selected).then((copied) => callbacksRef.current.onCopyResult(copied));
          }
        };
        host.addEventListener("keydown", keydown, true);
        syncGeometryRef.current = syncGeometry;

      })
      .catch((raw: unknown) => {
        callbacksRef.current.onError(toAppError(raw));
      });

    return () => {
      mounted = false;
      disposedRef.current = true;
      lifecycleRef.current += 1;
      if (pollTimerRef.current !== null) {
        window.clearTimeout(pollTimerRef.current);
        pollTimerRef.current = null;
      }
      schedulePullRef.current = null;
      wakeRequestedRef.current = false;
      inFlightRef.current = false;
      input?.dispose();
      if (keydown) hostRef.current?.removeEventListener("keydown", keydown, true);
      webgl?.dispose();
      term?.dispose();
      termRef.current = null;
      fitRef.current = null;
      syncGeometryRef.current = null;
    };
    // The pane is keyed by terminalId; a new id gets a fresh instance.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [terminalId]);

  const pull = useCallback(() => {
    if (disposedRef.current) return;
    if (inFlightRef.current) {
      wakeRequestedRef.current = true;
      return;
    }
    if (pollTimerRef.current !== null) {
      window.clearTimeout(pollTimerRef.current);
      pollTimerRef.current = null;
    }
    // Parsing one tiny chunk must not start another invoke immediately. All
    // wakeups, including native output events, share this monotonic deadline.
    // Bytes keep accumulating in the backend's bounded ring between pulls.
    const remaining = MIN_PULL_INTERVAL_MS - (performance.now() - lastPullAtRef.current);
    if (remaining > 0) {
      schedulePullRef.current?.(remaining);
      return;
    }
    const lifecycle = lifecycleRef.current;
    const isCurrent = () => !disposedRef.current && lifecycleRef.current === lifecycle;
    const finish = (delay: number | null) => {
      if (!isCurrent()) return;
      inFlightRef.current = false;
      if (wakeRequestedRef.current) {
        wakeRequestedRef.current = false;
        pull();
      } else if (delay !== null) {
        schedulePullRef.current?.(delay);
      } else {
        pull();
      }
    };
    // Keep the PTY geometry in step with the pane before reading more output.
    syncGeometryRef.current?.();
    lastPullAtRef.current = performance.now();
    inFlightRef.current = true;
    void api
      .terminalRead({
        terminalId,
        cursor: cursorRef.current,
        maxBytes: READ_BATCH,
      })
      .then((view) => {
        if (!isCurrent()) return;
        cursorRef.current = view.toSeq;
        // Only report a change: the parent's reducer then keeps the same object
        // and React skips a re-render for every read that changed nothing.
        const previous = lastStateRef.current;
        if (previous?.state !== view.state || previous?.exitCode !== (view.exit?.code ?? null)) {
          lastStateRef.current = { state: view.state, exitCode: view.exit?.code ?? null };
          callbacksRef.current.onState(view.state, view.exit);
        }
        if (view.historyTruncated) {
          // A first read that starts after a trimmed window has no
          // `droppedBefore`; report the accumulated total instead of "0 bytes".
          callbacksRef.current.onDropped(
            view.droppedBefore > 0 ? view.droppedBefore : view.droppedBytes,
            view.droppedBytes,
          );
        }
        const term = termRef.current;
        if (!term) {
          finish(100);
          return;
        }
        if (view.text) {
          // Backpressure: the next read starts only after xterm parsed this
          // batch. A slow renderer leaves bytes in the bounded ring, where they
          // are counted, instead of queueing an unbounded number of events.
          term.write(view.text, () => finish(null));
        } else {
          finish(view.state === "running" ? POLL_RUNNING_MS : POLL_ENDED_MS);
        }
      })
      .catch((raw: unknown) => {
        if (!isCurrent()) return;
        inFlightRef.current = false;
        const error = toAppError(raw);
        // `not_found` means the tab was closed elsewhere; stop polling.
        if (error.code === "not_found") return;
        callbacksRef.current.onError(error);
        schedulePullRef.current?.(400);
      });
  }, [api, terminalId]);

  useEffect(() => {
    const schedule = (delay: number) => {
      if (disposedRef.current || pollTimerRef.current !== null) return;
      pollTimerRef.current = window.setTimeout(() => {
        pollTimerRef.current = null;
        pull();
      }, delay);
    };
    schedulePullRef.current = schedule;
    return () => {
      if (pollTimerRef.current !== null) {
        window.clearTimeout(pollTimerRef.current);
        pollTimerRef.current = null;
      }
      if (schedulePullRef.current === schedule) schedulePullRef.current = null;
    };
  }, [pull]);

  // The backend emits a notification (no payload bytes) for every batch; it is
  // what makes the pull feel immediate. Polling above stays as the fallback for
  // a missed event.
  useEffect(() => {
    if (!ready) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<TerminalNotify>(TERMINAL_EVENT, (event) => {
      if (disposed) return;
      if (event.payload?.terminalId !== terminalId) return;
      pull();
    })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => {
        // Outside Tauri (jsdom) there is no event bridge; polling covers it.
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [ready, terminalId, pull]);

  useEffect(() => {
    if (!ready) return;
    const timer = window.setTimeout(pull, 0);
    return () => window.clearTimeout(timer);
  }, [ready, pull]);

  useEffect(() => {
    if (!active) return;
    // A tab hidden while the window changed size needs a refit and a geometry
    // sync on activation; forcing the cached size to zero makes the next read
    // tick do both through the single `syncGeometry` path.
    geometryRef.current = { width: 0, height: 0 };
    const term = termRef.current;
    const timer = window.setTimeout(() => term?.focus(), 0);
    return () => window.clearTimeout(timer);
  }, [active]);

  useEffect(() => {
    const term = termRef.current;
    if (!term || !ready) return;
    term.options.theme = dark ? DARK_THEME : LIGHT_THEME;
    term.options.fontSize = fontSize;
    geometryRef.current = { width: 0, height: 0 };
    syncGeometryRef.current?.();
  }, [dark, fontSize, ready]);

  return (
    <div
      className="relative h-full min-h-0 w-full overflow-hidden bg-background-primary-default"
      data-testid={`terminal-pane-${terminalId}`}
      onContextMenu={(event) => {
        // Right click copies the selection when there is one; with no selection
        // the browser default menu stays available.
        const selected = termRef.current?.getSelection() ?? "";
        if (!selected) return;
        event.preventDefault();
        void copyText(selected).then((copied) => callbacksRef.current.onCopyResult(copied));
      }}
    >
      <div ref={hostRef} className="absolute inset-0" />
      <p className="pointer-events-none absolute bottom-1 right-2 text-[10px] text-text-tertiary">
        {t("terminalPaneHint")}
      </p>
    </div>
  );
}
