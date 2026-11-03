// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import "@/i18n/multicli";

import type { MultiCliApi } from "../api";
import type {
  AppError,
  TerminalLimits,
  TerminalListView,
  TerminalOpenSpec,
  TerminalReadView,
  TerminalStopMode,
  TerminalStopView,
  TerminalTargetsView,
  TerminalView,
} from "../types";

// The xterm instance needs a real canvas and a real PTY; the workspace logic is
// what this suite covers, so the pane is replaced by a stub that also exposes
// the callbacks the workspace hands it.
vi.mock("./TerminalPane", () => ({
  default: (props: {
    terminalId: string;
    onDropped: (before: number, total: number) => void;
    onState: (state: string, exit: null) => void;
    onError: (error: unknown) => void;
  }) => (
    <div data-testid={`pane-${props.terminalId}`}>
      <button
        type="button"
        data-testid={`pane-drop-${props.terminalId}`}
        onClick={() => props.onDropped(1024, 4096)}
      />
      <button
        type="button"
        data-testid={`pane-error-${props.terminalId}`}
        onClick={() =>
          props.onError({
            code: "io",
            message: "the console writer is gone",
            retryable: true,
            details: { stage: "write" },
          })
        }
      />
    </div>
  ),
}));

import TerminalWorkspace from "./TerminalWorkspace";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const LIMITS: TerminalLimits = {
  bufferBytes: 4 * 1024 * 1024,
  minBufferBytes: 256 * 1024,
  maxBufferBytes: 64 * 1024 * 1024,
  logBytes: 20 * 1024 * 1024,
  minLogBytes: 1024 * 1024,
  maxLogBytes: 512 * 1024 * 1024,
  maxTerminals: 16,
  note: "limits",
};

const TARGETS: TerminalTargetsView = {
  schemaVersion: 1,
  items: [
    {
      id: "shell:cmd",
      kind: "shell",
      label: "cmd.exe",
      agentId: "local-shell",
      product: null,
      installationId: null,
      executable: {
        hostId: "local-windows",
        displayPath: "C:\\Windows\\System32\\cmd.exe",
        canonicalPath: "C:\\Windows\\System32\\cmd.exe",
      },
      launchChain: "exe",
      status: "ready",
      reason: null,
      version: null,
    },
    {
      id: "installation:inst-1",
      kind: "installation",
      label: "Codex · codex.cmd",
      agentId: "codex",
      product: "Codex",
      installationId: "inst-1",
      executable: {
        hostId: "local-windows",
        displayPath: "C:\\npm\\codex.cmd",
        canonicalPath: "C:\\npm\\codex.cmd",
      },
      launchChain: "cmd",
      status: "ready",
      reason: null,
      version: "0.166.0",
    },
    {
      id: "installation:inst-missing",
      kind: "installation",
      label: "Gone · gone.exe",
      agentId: "gone",
      product: "Gone",
      installationId: "inst-missing",
      executable: null,
      launchChain: "exe",
      status: "missing",
      reason: "可执行文件不存在；不会启动一个已失效的安装",
      version: null,
    },
  ],
  limits: LIMITS,
  cwdDefault: "C:\\Work\\中文 目录",
  note: "targets note",
};

function terminalView(overrides: Partial<TerminalView> = {}): TerminalView {
  return {
    schemaVersion: 1,
    terminalId: "term-1",
    runId: "run-1",
    sessionId: "session-1",
    agentId: "local-shell",
    installationId: null,
    title: "cmd.exe · 中文 目录",
    targetKind: "shell",
    product: null,
    executable: {
      hostId: "local-windows",
      displayPath: "C:\\Windows\\System32\\cmd.exe",
      canonicalPath: "C:\\Windows\\System32\\cmd.exe",
    },
    launchChain: "exe",
    launchArgv: ["C:\\Windows\\System32\\cmd.exe", "/d", "/q", "/k", "chcp 65001>nul"],
    args: [],
    cwd: {
      hostId: "local-windows",
      displayPath: "C:\\Work\\中文 目录",
      canonicalPath: "C:\\Work\\中文 目录",
    },
    envNames: [],
    state: "running",
    interactionStatus: "unknown",
    identity: {
      hostId: "local-windows",
      pid: 4242,
      creationTime: "2026-11-02T10:00:00.0000000Z",
    },
    job: { mode: "kill-on-close", assigned: true, reason: null },
    buffers: {
      bufferCapacityBytes: 4 * 1024 * 1024,
      bufferedBytes: 2048,
      droppedBytes: 0,
      logPath: "C:\\data\\logs\\terminal\\term-1.log",
      logCapacityBytes: 20 * 1024 * 1024,
      logBytes: 4096,
      logRotations: 0,
      logDroppedBytes: 0,
    },
    exit: null,
    stop: null,
    startedAt: "2026-11-02T10:00:00.000Z",
    endedAt: null,
    evidence: "multicli-terminal-v1",
    notes: [],
    ...overrides,
  };
}

class FakeTerminalApi {
  opened: TerminalOpenSpec[] = [];
  stopped: { terminalId: string; mode: TerminalStopMode }[] = [];
  closed: string[] = [];
  openFailure: AppError | null = null;
  views: Record<string, TerminalView> = {};

  terminalTargets = () => Promise.resolve(TARGETS);
  terminalOpen = (spec: TerminalOpenSpec) => {
    this.opened.push(spec);
    if (this.openFailure) return Promise.reject(this.openFailure);
    const view = terminalView();
    this.views[view.terminalId] = view;
    return Promise.resolve(view);
  };
  terminalList = (): Promise<TerminalListView> =>
    Promise.resolve({
      schemaVersion: 1,
      items: Object.values(this.views),
      limits: LIMITS,
      activeCount: Object.values(this.views).filter((item) => item.state === "running").length,
      note: "list",
    });
  terminalRead = (): Promise<TerminalReadView> =>
    Promise.reject({
      code: "not_found",
      message: "no stream in this fake",
      retryable: false,
      details: {},
    });
  terminalWrite = () => Promise.resolve({ terminalId: "term-1", acceptedBytes: 1, state: "running" });
  terminalResize = () =>
    Promise.resolve({ terminalId: "term-1", cols: 80, rows: 24 });
  terminalStop = (terminalId: string, mode: TerminalStopMode): Promise<TerminalStopView> => {
    this.stopped.push({ terminalId, mode });
    const previous = this.views[terminalId] ?? terminalView();
    // Ctrl+C interrupts the foreground job but leaves the shell running; a
    // gentle or forced stop ends the terminal.
    const nextState =
      mode === "interrupt"
        ? previous
        : {
            ...previous,
            state: "exited" as const,
            exit: {
              code: 0,
              success: true,
              endedAt: "2026-11-02T10:01:00.000Z",
              reported: true,
              signal: null,
            },
          };
    const report = {
      requested: mode,
      interruptSent: mode !== "force",
      escalated: mode === "gentle",
      waitedMs: 700,
      alreadyTerminal: false,
      refusedReason: null,
    };
    const next = { ...nextState, stop: report };
    this.views[terminalId] = next;
    return Promise.resolve({
      terminal: next,
      stop: report,
    });
  };
  terminalClose = (terminalId: string) => {
    this.closed.push(terminalId);
    delete this.views[terminalId];
    return Promise.resolve({
      terminalId,
      removed: true,
      stop: {
        requested: "gentle" as const,
        interruptSent: true,
        escalated: false,
        waitedMs: 10,
        alreadyTerminal: false,
        refusedReason: null,
      },
      note: "closed",
    });
  };
  terminalShutdownAll = () =>
    Promise.resolve({ stopped: [], remaining: 0, note: "nothing" });
  terminalLimits = () => Promise.resolve(LIMITS);
  terminalQuitCheck = () =>
    Promise.resolve({ activeCount: 0, needsConfirmation: false, note: "none" });
  appQuit = () => Promise.resolve();
}

function byTestId(container: HTMLElement, id: string): HTMLElement | null {
  return container.querySelector(`[data-testid="${id}"]`);
}

async function flush(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function click(node: HTMLElement | null): Promise<void> {
  expect(node).not.toBeNull();
  await act(async () => {
    node?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
}

async function setValue(node: HTMLInputElement | HTMLTextAreaElement | null, value: string) {
  expect(node).not.toBeNull();
  if (!node) return;
  const setter = Object.getOwnPropertyDescriptor(
    node instanceof HTMLTextAreaElement
      ? HTMLTextAreaElement.prototype
      : HTMLInputElement.prototype,
    "value",
  )?.set;
  await act(async () => {
    setter?.call(node, value);
    node.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

let rendered: { root: Root; container: HTMLElement } | null = null;

beforeAll(async () => {
  if (!i18n.isInitialized) {
    await new Promise<void>((resolve) => {
      i18n.on("initialized", () => resolve());
    });
  }
  await i18n.loadNamespaces("multicli");
});

afterEach(() => {
  if (rendered) {
    act(() => rendered?.root.unmount());
    rendered.container.remove();
    rendered = null;
  }
});

async function renderWorkspace(fake: FakeTerminalApi) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  rendered = { root, container };
  await act(async () => {
    root.render(<TerminalWorkspace api={fake as unknown as MultiCliApi} />);
  });
  await flush();
  return container;
}

describe("managed terminal workspace", () => {
  it("opens a terminal with a structured spec and keeps the tab", async () => {
    const fake = new FakeTerminalApi();
    const container = await renderWorkspace(fake);
    expect(byTestId(container, "terminal-empty")).not.toBeNull();

    await click(byTestId(container, "terminal-new"));
    expect(byTestId(container, "terminal-new-dialog")).not.toBeNull();

    await setValue(byTestId(container, "terminal-cwd-input") as HTMLInputElement, "C:\\Work\\中文 目录");
    await click(byTestId(container, "terminal-dialog-create"));

    expect(fake.opened).toHaveLength(1);
    const spec = fake.opened[0];
    expect(spec.target).toEqual({ kind: "shell", shell: "cmd", installationId: null });
    expect(spec.args).toEqual([]);
    expect(spec.cwd).toBe("C:\\Work\\中文 目录");
    expect(spec.env).toEqual([]);
    expect(spec.cols).toBeGreaterThan(0);
    expect(spec.rows).toBeGreaterThan(0);

    expect(byTestId(container, "terminal-tab-term-1")).not.toBeNull();
    expect(byTestId(container, "pane-term-1")).not.toBeNull();
    expect(byTestId(container, "terminal-new-dialog")).toBeNull();
  });

  it("passes one argv entry per argument line and rejects malformed env lines", async () => {
    const fake = new FakeTerminalApi();
    const container = await renderWorkspace(fake);
    await click(byTestId(container, "terminal-new"));

    const select = byTestId(container, "terminal-target-select") as HTMLSelectElement;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set;
      setter?.call(select, "installation:inst-1");
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await flush();

    await setValue(
      byTestId(container, "terminal-args-input") as HTMLTextAreaElement,
      "a b\nc&d\n\n\"quoted\"",
    );
    await setValue(
      byTestId(container, "terminal-env-input") as HTMLTextAreaElement,
      "MULTICLI_S04=1\nNOT A PAIR",
    );
    await click(byTestId(container, "terminal-dialog-create"));
    expect(byTestId(container, "terminal-dialog-error")).not.toBeNull();
    expect(fake.opened).toHaveLength(0);

    await setValue(
      byTestId(container, "terminal-env-input") as HTMLTextAreaElement,
      "MULTICLI_S04=1",
    );
    await click(byTestId(container, "terminal-dialog-create"));
    expect(fake.opened).toHaveLength(1);
    expect(fake.opened[0].target).toEqual({
      kind: "installation",
      shell: null,
      installationId: "inst-1",
    });
    expect(fake.opened[0].args).toEqual(["a b", "c&d", "\"quoted\""]);
    expect(fake.opened[0].env).toEqual([{ name: "MULTICLI_S04", value: "1" }]);
  });

  it("shows a real AppError when the target cannot be started", async () => {
    const fake = new FakeTerminalApi();
    fake.openFailure = {
      code: "unsupported",
      message: "the owned process tree could not be constrained",
      retryable: false,
      details: { stage: "jobAssignment" },
    };
    const container = await renderWorkspace(fake);
    await click(byTestId(container, "terminal-new"));
    await click(byTestId(container, "terminal-dialog-create"));

    expect(byTestId(container, "terminal-error")).not.toBeNull();
    expect(byTestId(container, "terminal-error-code")?.textContent).toBe("unsupported");
    expect(byTestId(container, "terminal-new-dialog")).not.toBeNull();
  });

  it("separates Ctrl+C, gentle stop and force stop and reports escalation", async () => {
    const fake = new FakeTerminalApi();
    const container = await renderWorkspace(fake);
    await click(byTestId(container, "terminal-new"));
    await click(byTestId(container, "terminal-dialog-create"));

    await click(byTestId(container, "terminal-stop-interrupt"));
    expect(fake.stopped.at(-1)).toEqual({ terminalId: "term-1", mode: "interrupt" });

    await click(byTestId(container, "terminal-stop-gentle"));
    expect(fake.stopped.at(-1)).toEqual({ terminalId: "term-1", mode: "gentle" });
    expect(byTestId(container, "terminal-notice")?.textContent).toContain("700");
    expect(byTestId(container, "terminal-status-stop")?.textContent).toContain("温和停止");
    // After the exit the stop buttons are disabled: no repeated signalling.
    expect((byTestId(container, "terminal-stop-force") as HTMLButtonElement).disabled).toBe(true);
  });

  it("surfaces dropped history and a pane error instead of hiding them", async () => {
    const fake = new FakeTerminalApi();
    const container = await renderWorkspace(fake);
    await click(byTestId(container, "terminal-new"));
    await click(byTestId(container, "terminal-dialog-create"));

    await click(byTestId(container, "pane-drop-term-1"));
    expect(byTestId(container, "terminal-drop-notice")?.textContent).toContain("1.0 KiB");
    await click(byTestId(container, "terminal-drop-dismiss"));
    expect(byTestId(container, "terminal-drop-notice")).toBeNull();

    await click(byTestId(container, "pane-error-term-1"));
    expect(byTestId(container, "terminal-error-code")?.textContent).toBe("io");
  });

  it("closes only the selected tab and forgets its view", async () => {
    const fake = new FakeTerminalApi();
    const container = await renderWorkspace(fake);
    await click(byTestId(container, "terminal-new"));
    await click(byTestId(container, "terminal-dialog-create"));
    expect(byTestId(container, "terminal-tab-term-1")).not.toBeNull();

    await click(byTestId(container, "terminal-close"));
    expect(fake.closed).toEqual(["term-1"]);
    expect(byTestId(container, "terminal-tab-term-1")).toBeNull();
    expect(byTestId(container, "terminal-empty")).not.toBeNull();
  });

  it("does not allow creating a terminal from a missing target", async () => {
    const fake = new FakeTerminalApi();
    const container = await renderWorkspace(fake);
    await click(byTestId(container, "terminal-new"));

    const select = byTestId(container, "terminal-target-select") as HTMLSelectElement;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set;
      setter?.call(select, "installation:inst-missing");
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await flush();
    expect(byTestId(container, "terminal-target-reason")).not.toBeNull();
    await click(byTestId(container, "terminal-dialog-create"));
    expect(byTestId(container, "terminal-dialog-error")).not.toBeNull();
    expect(fake.opened).toHaveLength(0);
  });
});
