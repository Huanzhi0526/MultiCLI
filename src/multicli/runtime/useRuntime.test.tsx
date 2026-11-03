// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";

import { useRuntime } from "./useRuntime";
import type { MultiCliApi } from "../api";
import type { RuntimeRunView } from "../types";

vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => undefined }));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

interface Deferred<T> {
  promise: Promise<T>;
  settle: (value: T) => void;
}

function defer<T>(): Deferred<T> {
  let settle!: (value: T) => void;
  const promise = new Promise<T>((resolve) => {
    settle = resolve;
  });
  return { promise, settle };
}

function runView(runId: string, text: string): RuntimeRunView {
  return { runId, messages: [{ text }] } as unknown as RuntimeRunView;
}

function harness(api: MultiCliApi) {
  let current!: ReturnType<typeof useRuntime>;
  function Probe() {
    current = useRuntime(api);
    return null;
  }
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  return {
    get state() {
      return current;
    },
    async mount() {
      await act(async () => {
        root.render(<Probe />);
      });
    },
    async unmount() {
      await act(async () => root.unmount());
      host.remove();
    },
  };
}

it("a late read for the previously selected run never replaces the current one", async () => {
  const lateA = defer<RuntimeRunView>();
  const a = runView("A", "transcript-A");
  const b = runView("B", "transcript-B");
  const api = {
    runtimeCapabilities: vi.fn(async () => ({ items: [] })),
    runtimeList: vi.fn(async () => ({ items: [a, b] })),
    runtimeGet: vi.fn((runId: string) =>
      runId === "A" ? lateA.promise : Promise.resolve(b),
    ),
  } as unknown as MultiCliApi;

  const app = harness(api);
  try {
    await app.mount();
    await act(async () => app.state.selectRun("A"));
    await act(async () => app.state.selectRun("B"));
    expect(app.state.activeRunId).toBe("B");
    expect(app.state.run?.runId).toBe("B");

    await act(async () => lateA.settle(a));

    expect(app.state.activeRunId).toBe("B");
    expect(app.state.run?.runId).toBe("B");
    expect(app.state.run?.messages[0]?.text).toBe("transcript-B");
  } finally {
    await app.unmount();
  }
});

it("a response for a background run refreshes its list row but not the open transcript", async () => {
  const lateA = defer<RuntimeRunView>();
  const a = runView("A", "stale-A");
  const b = runView("B", "transcript-B");
  const api = {
    runtimeCapabilities: vi.fn(async () => ({ items: [] })),
    runtimeList: vi.fn(async () => ({ items: [a, b] })),
    runtimeGet: vi.fn((runId: string) =>
      runId === "A" ? lateA.promise : Promise.resolve(b),
    ),
  } as unknown as MultiCliApi;

  const app = harness(api);
  try {
    await app.mount();
    await act(async () => app.state.selectRun("A"));
    await act(async () => app.state.selectRun("B"));

    await act(async () => lateA.settle(runView("A", "fresh-A")));

    // The cached row for A did move forward...
    const cachedA = app.state.runs.find((item) => item.runId === "A");
    expect(cachedA?.messages[0]?.text).toBe("fresh-A");
    // ...while the visible run stayed on B.
    expect(app.state.activeRunId).toBe("B");
    expect(app.state.run?.messages[0]?.text).toBe("transcript-B");
  } finally {
    await app.unmount();
  }
});

it("a send that settles after the user switched runs does not rewrite the transcript", async () => {
  const pendingSend = defer<RuntimeRunView>();
  const a = runView("A", "transcript-A");
  const b = runView("B", "transcript-B");
  const api = {
    runtimeCapabilities: vi.fn(async () => ({ items: [] })),
    runtimeList: vi.fn(async () => ({ items: [a, b] })),
    runtimeGet: vi.fn(async (runId: string) => (runId === "A" ? a : b)),
    runtimeSend: vi.fn(() => pendingSend.promise),
  } as unknown as MultiCliApi;

  const app = harness(api);
  try {
    await app.mount();
    await act(async () => app.state.selectRun("A"));
    expect(app.state.run?.runId).toBe("A");

    await act(async () => {
      void app.state.send("hello A");
    });
    expect(api.runtimeSend).toHaveBeenCalledWith("A", "hello A");

    await act(async () => app.state.selectRun("B"));
    expect(app.state.run?.runId).toBe("B");

    await act(async () => pendingSend.settle(runView("A", "A answered late")));

    expect(app.state.activeRunId).toBe("B");
    expect(app.state.run?.runId).toBe("B");
    expect(app.state.run?.messages[0]?.text).toBe("transcript-B");
    // The late answer still landed in A's cached row.
    expect(app.state.runs.find((item) => item.runId === "A")?.messages[0]?.text).toBe(
      "A answered late",
    );
  } finally {
    await app.unmount();
  }
});
