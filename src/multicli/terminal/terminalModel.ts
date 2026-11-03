/**
 * Pure helpers for the managed terminal workspace. Everything here is
 * dependency-free so the tab model, the argument list and the drop notice can
 * be tested in jsdom without xterm or a real PTY.
 */

import type {
  ShellFlavor,
  TerminalLaunchChain,
  TerminalRunState,
  TerminalStopMode,
} from "../types";

export interface TerminalTab {
  terminalId: string;
  runId: string;
  title: string;
  chain: TerminalLaunchChain;
  state: TerminalRunState;
  exitCode: number | null;
  /** Set when the backend refused a stop (for example a reused PID). */
  stopRefused: string | null;
  /** `Date.now()` at creation, so a stale status poll cannot drop a new tab. */
  createdAt: number;
}

export type TabAction =
  | { type: "add"; tab: TerminalTab }
  | { type: "remove"; terminalId: string }
  | { type: "state"; terminalId: string; state: TerminalRunState; exitCode: number | null }
  | { type: "refused"; terminalId: string; reason: string | null }
  | { type: "activate"; terminalId: string };

export interface TabModel {
  tabs: TerminalTab[];
  activeId: string | null;
}

export function addTab(model: TabModel, tab: TerminalTab): TabModel {
  return { tabs: [...model.tabs, tab], activeId: tab.terminalId };
}

export function removeTab(model: TabModel, terminalId: string): TabModel {
  const index = model.tabs.findIndex((tab) => tab.terminalId === terminalId);
  const tabs = model.tabs.filter((tab) => tab.terminalId !== terminalId);
  if (model.activeId !== terminalId) {
    return { tabs, activeId: model.activeId };
  }
  const next = tabs[Math.min(Math.max(index, 0), tabs.length - 1)];
  return { tabs, activeId: next ? next.terminalId : null };
}

export function reduceTabs(model: TabModel, action: TabAction): TabModel {
  switch (action.type) {
    case "add":
      return addTab(model, action.tab);
    case "remove":
      return removeTab(model, action.terminalId);
    case "activate":
      return model.tabs.some((tab) => tab.terminalId === action.terminalId)
        ? { tabs: model.tabs, activeId: action.terminalId }
        : model;
    case "state": {
      const current = model.tabs.find((tab) => tab.terminalId === action.terminalId);
      if (!current) return model;
      // Identity matters: returning the same object lets React skip a re-render
      // for the many reads that do not change anything.
      if (current.state === action.state && current.exitCode === action.exitCode) {
        return model;
      }
      return {
        tabs: model.tabs.map((tab) =>
          tab.terminalId === action.terminalId
            ? { ...tab, state: action.state, exitCode: action.exitCode }
            : tab,
        ),
        activeId: model.activeId,
      };
    }
    case "refused":
      return {
        tabs: model.tabs.map((tab) =>
          tab.terminalId === action.terminalId
            ? { ...tab, stopRefused: action.reason }
            : tab,
        ),
        activeId: model.activeId,
      };
    default:
      return model;
  }
}

export const TERMINAL_STATE_LABEL: Record<TerminalRunState, string> = {
  starting: "terminalState.starting",
  running: "terminalState.running",
  stopping: "terminalState.stopping",
  exited: "terminalState.exited",
  failed: "terminalState.failed",
};

export const CHAIN_LABEL: Record<TerminalLaunchChain, string> = {
  exe: "terminalChain.exe",
  cmd: "terminalChain.cmd",
  powershell: "terminalChain.powershell",
};

export const SHELL_LABEL: Record<ShellFlavor, string> = {
  cmd: "cmd.exe",
  "windows-powershell": "Windows PowerShell",
  pwsh: "PowerShell 7 (pwsh)",
};

export const STOP_MODE_LABEL: Record<TerminalStopMode, string> = {
  interrupt: "terminalStop.interrupt",
  gentle: "terminalStop.gentle",
  force: "terminalStop.force",
};

export function isLive(state: TerminalRunState): boolean {
  return state === "starting" || state === "running" || state === "stopping";
}

/**
 * Arguments are typed one per line. A line is one opaque argv element: the UI
 * never splits on spaces, so a value that contains a space or a quote is passed
 * exactly as typed. Blank lines are ignored; an intentionally empty argument is
 * not expressible from this field, which the dialog states.
 */
export function parseArgsInput(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.replace(/\r$/, ""))
    .filter((line) => line.length > 0);
}

export interface ParsedEnv {
  env: { name: string; value: string }[];
  invalid: string[];
}

export function parseEnvInput(text: string): ParsedEnv {
  const env: { name: string; value: string }[] = [];
  const invalid: string[] = [];
  const seen = new Set<string>();
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line) continue;
    const index = line.indexOf("=");
    if (index <= 0) {
      invalid.push(line);
      continue;
    }
    const name = line.slice(0, index).trim();
    const value = line.slice(index + 1);
    if (!name || seen.has(name.toLowerCase())) {
      invalid.push(line);
      continue;
    }
    seen.add(name.toLowerCase());
    env.push({ name, value });
  }
  return { env, invalid };
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  const kib = bytes / 1024;
  if (kib < 1024) return `${kib.toFixed(kib < 10 ? 1 : 0)} KiB`;
  const mib = kib / 1024;
  if (mib < 1024) return `${mib.toFixed(mib < 10 ? 1 : 0)} MiB`;
  return `${(mib / 1024).toFixed(2)} GiB`;
}

export interface DropNotice {
  before: number;
  total: number;
}

/** A visible notice whenever retained output was discarded. */
export function dropNotice(droppedBefore: number, totalDropped: number): DropNotice | null {
  if (droppedBefore <= 0) return null;
  return { before: droppedBefore, total: totalDropped };
}

/** Title for a new tab: the target label plus the working directory leaf. */
export function tabTitle(targetLabel: string, cwd: string): string {
  const parts = cwd.split(/[\\/]/).filter(Boolean);
  const leaf = parts.length > 0 ? parts[parts.length - 1] : cwd;
  return leaf ? `${targetLabel} · ${leaf}` : targetLabel;
}
