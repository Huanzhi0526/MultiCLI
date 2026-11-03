// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@/lib/i18n";
import "@/i18n/multicli";

const harness = vi.hoisted(() => ({
  listener: null as ((event: { payload?: { terminalId?: string } }) => void) | null,
  onData: null as ((data: string) => void) | null,
  writes: [] as Array<{ text: string; callback: () => void }>,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (_event: string, listener: typeof harness.listener) => {
    harness.listener = listener;
    return () => {
      harness.listener = null;
    };
  },
}));

vi.mock("./xtermLoader", () => ({
  loadXterm: async () => ({
    Terminal: class {
      cols = 80;
      rows = 24;
      loadAddon() {}
      open() {}
      dispose() {}
      focus() {}
      getSelection() { return ""; }
      onData(callback: (data: string) => void) {
        harness.onData = callback;
        return { dispose: () => { harness.onData = null; } };
      }
      write(text: string, callback: () => void) {
        harness.writes.push({ text, callback });
      }
    },
    FitAddon: class { fit() {} },
    WebglAddon: class { onContextLoss() {} dispose() {} },
  }),
}));

import TerminalPane from "./TerminalPane";
import type { MultiCliApi } from "../api";
import type { TerminalReadView } from "../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLDivElement;

function readView(text: string, toSeq: number): TerminalReadView {
  return {
    terminalId: "term",
    runId: "run",
    streamId: "stream",
    fromSeq: toSeq - 1,
    toSeq,
    text,
    bytes: text.length,
    droppedBefore: 0,
    droppedBytes: 0,
    historyTruncated: false,
    bufferedBytes: 0,
    state: "running",
    exit: null,
  };
}

async function mount(api: MultiCliApi, onError = vi.fn()) {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => {
    root!.render(
      <TerminalPane
        api={api}
        terminalId="term"
        active={false}
        onState={() => {}}
        onDropped={() => {}}
        onError={onError}
        onCopyResult={() => {}}
      />,
    );
  });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
  return onError;
}

beforeEach(() => {
  vi.useFakeTimers();
  harness.listener = null;
  harness.onData = null;
  harness.writes = [];
});

afterEach(async () => {
  if (root) await act(async () => root!.unmount());
  root = null;
  document.body.innerHTML = "";
  vi.useRealTimers();
});

describe("TerminalPane asynchronous flow control", () => {
  it("batches a continuous stream of tiny output notifications without starving input tasks", async () => {
    let seq = 0;
    const terminalRead = vi.fn(async () => readView("tiny", ++seq));
    const api = {
      terminalRead,
      terminalResize: vi.fn(async () => ({})),
      terminalWrite: vi.fn(async () => ({})),
    } as unknown as MultiCliApi;
    await mount(api);
    let parsed = 0;
    for (let tick = 0; tick < 100; tick += 1) {
      await act(async () => {
        while (parsed < harness.writes.length) harness.writes[parsed++].callback();
        harness.listener?.({ payload: { terminalId: "term" } });
        await vi.advanceTimersByTimeAsync(1);
      });
    }
    expect(terminalRead.mock.calls.length).toBeGreaterThan(1);
    expect(terminalRead.mock.calls.length).toBeLessThanOrEqual(4);
    expect(vi.getTimerCount()).toBeLessThanOrEqual(1);
  });

  it("keeps reads serialized through xterm parsing and merges output notifications", async () => {
    let seq = 0;
    const terminalRead = vi.fn(async () => readView("chunk", ++seq));
    const api = {
      terminalRead,
      terminalResize: vi.fn(async () => ({})),
      terminalWrite: vi.fn(async () => ({})),
    } as unknown as MultiCliApi;
    await mount(api);

    expect(terminalRead).toHaveBeenCalledTimes(1);
    expect(harness.writes).toHaveLength(1);
    for (let index = 0; index < 20; index += 1) {
      await act(async () => harness.listener?.({ payload: { terminalId: "term" } }));
    }
    expect(terminalRead).toHaveBeenCalledTimes(1);

    await act(async () => harness.writes[0].callback());
    expect(terminalRead).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(32); });
    expect(terminalRead).toHaveBeenCalledTimes(2);
    expect(harness.writes).toHaveLength(2);
    await act(async () => harness.writes[1].callback());
    await act(async () => { await vi.advanceTimersByTimeAsync(32); });
    expect(terminalRead).toHaveBeenCalledTimes(3);
  });

  it("coalesces notifications during an idle read and leaves one poll timer", async () => {
    let resolveRead!: (view: TerminalReadView) => void;
    const terminalRead = vi.fn(() =>
      new Promise<TerminalReadView>((resolve) => { resolveRead = resolve; }),
    );
    const api = {
      terminalRead,
      terminalResize: vi.fn(async () => ({})),
      terminalWrite: vi.fn(async () => ({})),
    } as unknown as MultiCliApi;
    await mount(api);

    for (let index = 0; index < 20; index += 1) {
      await act(async () => harness.listener?.({ payload: { terminalId: "term" } }));
    }
    expect(terminalRead).toHaveBeenCalledTimes(1);
    await act(async () => resolveRead(readView("", 0)));
    expect(terminalRead).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(32); });
    expect(terminalRead).toHaveBeenCalledTimes(2);
    await act(async () => resolveRead(readView("", 0)));
    expect(vi.getTimerCount()).toBe(1);
  });

  it("ignores a late read response after unmount and clears its poll timer", async () => {
    let resolveRead!: (view: TerminalReadView) => void;
    const terminalRead = vi.fn(() =>
      new Promise<TerminalReadView>((resolve) => { resolveRead = resolve; }),
    );
    const api = {
      terminalRead,
      terminalResize: vi.fn(async () => ({})),
      terminalWrite: vi.fn(async () => ({})),
    } as unknown as MultiCliApi;
    await mount(api);

    await act(async () => root!.unmount());
    root = null;
    await act(async () => resolveRead(readView("late", 1)));
    expect(harness.writes).toHaveLength(0);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("ignores an xterm parse callback that arrives after unmount", async () => {
    const api = {
      terminalRead: vi.fn(async () => readView("late parse", 1)),
      terminalResize: vi.fn(async () => ({})),
      terminalWrite: vi.fn(async () => ({})),
    } as unknown as MultiCliApi;
    await mount(api);
    expect(harness.writes).toHaveLength(1);

    await act(async () => root!.unmount());
    root = null;
    await act(async () => harness.writes[0].callback());
    expect(api.terminalRead).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("serializes typed input and rejects data beyond the bounded queue", async () => {
    let releaseFirst!: () => void;
    const firstWrite = new Promise<Record<string, never>>((resolve) => {
      releaseFirst = () => resolve({});
    });
    const terminalWrite = vi.fn()
      .mockReturnValueOnce(firstWrite)
      .mockResolvedValue({});
    const api = {
      terminalRead: vi.fn(async () => readView("", 0)),
      terminalResize: vi.fn(async () => ({})),
      terminalWrite,
    } as unknown as MultiCliApi;
    const onError = await mount(api);

    await act(async () => harness.onData?.("first"));
    await act(async () => harness.onData?.("second"));
    expect(terminalWrite.mock.calls.map(([, data]) => data)).toEqual(["first"]);
    await act(async () => releaseFirst());
    expect(terminalWrite.mock.calls.map(([, data]) => data)).toEqual(["first", "second"]);

    let releaseHuge!: () => void;
    const hugeWrite = new Promise<Record<string, never>>((resolve) => {
      releaseHuge = () => resolve({});
    });
    terminalWrite.mockImplementationOnce(() => hugeWrite);
    await act(async () => harness.onData?.("x".repeat(1024 * 1024)));
    await act(async () => harness.onData?.("overflow"));
    expect(onError).toHaveBeenCalledWith(expect.objectContaining({
      code: "protocol",
      message: expect.stringContaining("1 MiB"),
    }));
    expect(terminalWrite.mock.calls.at(-1)?.[1]).toBe("x".repeat(1024 * 1024));
    await act(async () => releaseHuge());
  });
});
