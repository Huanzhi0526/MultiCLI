import { describe, expect, it } from "vitest";

import { createMultiCliApi } from "../api";
import {
  CHAIN_LABEL,
  addTab,
  dropNotice,
  formatBytes,
  isLive,
  parseArgsInput,
  parseEnvInput,
  reduceTabs,
  removeTab,
  tabTitle,
  TERMINAL_STATE_LABEL,
} from "./terminalModel";
import type { TabModel, TerminalTab } from "./terminalModel";

const tab = (id: string): TerminalTab => ({
  terminalId: id,
  runId: `run-${id}`,
  title: `title-${id}`,
  chain: "exe",
  state: "running",
  exitCode: null,
  stopRefused: null,
  createdAt: 0,
});

describe("terminal tab model", () => {
  it("activates a new tab and falls back to a neighbour when it closes", () => {
    let model: TabModel = { tabs: [], activeId: null };
    model = reduceTabs(model, { type: "add", tab: tab("a") });
    model = reduceTabs(model, { type: "add", tab: tab("b") });
    expect(model.activeId).toBe("b");

    model = reduceTabs(model, { type: "activate", terminalId: "a" });
    expect(model.activeId).toBe("a");

    // Closing an inactive tab keeps the active one.
    model = reduceTabs(model, { type: "remove", terminalId: "b" });
    expect(model.tabs.map((item) => item.terminalId)).toEqual(["a"]);
    expect(model.activeId).toBe("a");

    model = reduceTabs(model, { type: "remove", terminalId: "a" });
    expect(model.tabs).toEqual([]);
    expect(model.activeId).toBeNull();
  });

  it("records state, exit code and a refused stop without dropping the tab", () => {
    const model = addTab({ tabs: [], activeId: null }, tab("a"));
    const exited = reduceTabs(model, {
      type: "state",
      terminalId: "a",
      state: "exited",
      exitCode: 3,
    });
    expect(exited.tabs[0].state).toBe("exited");
    expect(exited.tabs[0].exitCode).toBe(3);
    const refused = reduceTabs(exited, {
      type: "refused",
      terminalId: "a",
      reason: "pid now belongs to a different process",
    });
    expect(refused.tabs[0].stopRefused).toContain("different process");
    expect(refused.tabs).toHaveLength(1);
  });

  it("keeps the active tab when an unknown id is activated or removed", () => {
    const model = addTab({ tabs: [], activeId: null }, tab("a"));
    expect(reduceTabs(model, { type: "activate", terminalId: "zzz" }).activeId).toBe("a");
    expect(removeTab(model, "zzz").tabs).toHaveLength(1);
  });

  it("treats only starting/running/stopping as live", () => {
    expect(isLive("running")).toBe(true);
    expect(isLive("starting")).toBe(true);
    expect(isLive("stopping")).toBe(true);
    expect(isLive("exited")).toBe(false);
    expect(isLive("failed")).toBe(false);
  });

  it("has a label key for every state and chain", () => {
    for (const state of ["starting", "running", "stopping", "exited", "failed"] as const) {
      expect(TERMINAL_STATE_LABEL[state]).toMatch(/^terminalState\./);
    }
    for (const chain of ["exe", "cmd", "powershell"] as const) {
      expect(CHAIN_LABEL[chain]).toMatch(/^terminalChain\./);
    }
  });

  it("derives a readable tab title from the target and the cwd leaf", () => {
    expect(tabTitle("cmd.exe", "C:\\Work\\中文 目录")).toBe("cmd.exe · 中文 目录");
    expect(tabTitle("cmd.exe", "C:\\Work\\中文 目录\\")).toBe("cmd.exe · 中文 目录");
    expect(tabTitle("target", "")).toBe("target");
  });
});

describe("terminal argument input", () => {
  it("treats one line as one opaque argv entry, never splitting on spaces", () => {
    expect(parseArgsInput("a b\nc&d\n\"quoted value\"\n中文")).toEqual([
      "a b",
      "c&d",
      '"quoted value"',
      "中文",
    ]);
  });

  it("ignores blank lines and normalizes CRLF", () => {
    expect(parseArgsInput("a\r\n\r\nb\r\n")).toEqual(["a", "b"]);
    expect(parseArgsInput("")).toEqual([]);
  });

  it("parses KEY=VALUE lines and reports malformed ones", () => {
    const parsed = parseEnvInput("A=1\nB=x=y\n\nC\n=D\nA=2");
    expect(parsed.env).toEqual([
      { name: "A", value: "1" },
      { name: "B", value: "x=y" },
    ]);
    expect(parsed.invalid).toEqual(["C", "=D", "A=2"]);
  });
});

describe("terminal notices", () => {
  it("formats byte sizes without lying about precision", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1.0 KiB");
    expect(formatBytes(1536)).toBe("1.5 KiB");
    expect(formatBytes(4 * 1024 * 1024)).toBe("4.0 MiB");
    expect(formatBytes(20 * 1024 * 1024)).toBe("20 MiB");
  });

  it("only produces a drop notice when bytes were actually discarded", () => {
    expect(dropNotice(0, 4096)).toBeNull();
    expect(dropNotice(1024, 4096)).toEqual({ before: 1024, total: 4096 });
  });
});

describe("terminal IPC argument shape", () => {
  it("sends a structured spec, a bounded read and an explicit stop mode", async () => {
    const calls: { command: string; args: Record<string, unknown> | undefined }[] = [];
    const api = createMultiCliApi(<T,>(command: string, args?: Record<string, unknown>) => {
      calls.push({ command, args });
      return Promise.resolve({} as T);
    });

    await api.terminalOpen({
      target: { kind: "shell", shell: "cmd", installationId: null },
      args: ["a b"],
      cwd: "C:\\Work",
      env: [{ name: "A", value: "1" }],
      cols: 120,
      rows: 32,
      title: "t",
    });
    expect(calls.at(-1)?.command).toBe("multicli_terminal_open");
    expect(calls.at(-1)?.args).toEqual({
      spec: {
        target: { kind: "shell", shell: "cmd", installationId: null },
        args: ["a b"],
        cwd: "C:\\Work",
        env: [{ name: "A", value: "1" }],
        cols: 120,
        rows: 32,
        title: "t",
      },
    });

    await api.terminalRead({ terminalId: "term-1", cursor: 0, maxBytes: 1024 });
    expect(calls.at(-1)?.args).toEqual({
      query: { terminalId: "term-1", cursor: 0, maxBytes: 1024 },
    });
    await api.terminalRead({ terminalId: "term-1" });
    expect(calls.at(-1)?.args).toEqual({ query: { terminalId: "term-1" } });

    await api.terminalStop("term-1", "gentle", 1500);
    expect(calls.at(-1)?.args).toEqual({
      query: { terminalId: "term-1", mode: "gentle", graceMs: 1500 },
    });

    await api.terminalClose("term-1");
    expect(calls.at(-1)?.args).toEqual({ query: { terminalId: "term-1" } });

    await api.appQuit();
    expect(calls.at(-1)?.command).toBe("multicli_app_quit");
    expect(calls.at(-1)?.args).toEqual({ confirm: true });
  });
});
