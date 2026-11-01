// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import i18n from "@/lib/i18n";
import App from "./App";
import type { MultiCliApi } from "./api";
import type {
  AgentListItem,
  Capability,
  CapabilityName,
  CapabilityQuery,
  DiagnosticsExport,
  Page,
  SessionDetail,
  SessionQuery,
  SessionSummary,
  WorkspaceListItem,
} from "./types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

beforeAll(async () => {
  if (!i18n.isInitialized) {
    await new Promise<void>((resolve) => {
      i18n.on("initialized", () => resolve());
    });
  }
  await i18n.loadNamespaces("multicli");
});

const workspace = (id: string, path: string): WorkspaceListItem => ({
  workspace: {
    id,
    path: { hostId: "local-windows", displayPath: path, canonicalPath: path },
    repoGroupId: "git-demo",
    worktreeId: `wt-${id}`,
  },
  sessionCount: 1,
});

const agent = (id: string, displayName: string): AgentListItem => ({
  agent: { id, vendor: "synthetic", displayName },
  installationCount: 1,
  sessionCount: 1,
});

const summary = (id: string, agentId: string, title: string): SessionSummary => ({
  session: {
    id,
    agentId,
    hostId: "local-windows",
    sourceNamespace: `${agentId}/fixture/local-windows`,
    nativeId: "native-secret",
    origin: "managed",
    title,
  },
  primaryWorkspaceId: "ws-1",
  workspaceIds: ["ws-1", "ws-2"],
  messageCount: 2,
  runCount: 1,
});

const detail = (id: string, agentId: string, title: string): SessionDetail => ({
  session: {
    id,
    agentId,
    hostId: "local-windows",
    sourceNamespace: `${agentId}/fixture/local-windows`,
    nativeId: "native-secret",
    origin: "managed",
    title,
  },
  workspaces: [
    { sessionId: id, workspaceId: "ws-1", role: "primary", evidence: "fixture primary" },
    { sessionId: id, workspaceId: "ws-2", role: "related", evidence: "fixture linked worktree" },
  ],
  runs: [
    {
      id: "run-1",
      sessionId: id,
      installationId: null,
      transport: "native",
      ownership: "managed",
      startedAt: "2026-11-01T09:00:00.000Z",
      endedAt: null,
      executionStatus: "running",
      interactionStatus: "working",
      evidenceLevel: "observed",
      evidence: "fixture",
    },
  ],
  messages: [
    {
      id: "m-1",
      sessionId: id,
      runId: "run-1",
      nativeEventId: "native-event",
      sourceKey: "fixture:1",
      seq: 0,
      kind: "assistant-message",
      content: { text: "first message" },
      sourceRef: "fixture",
      observedAt: "2026-11-01T09:00:01.000Z",
    },
    {
      id: "m-2",
      sessionId: id,
      runId: "run-1",
      nativeEventId: "native-event-2",
      sourceKey: "fixture:2",
      seq: 1,
      kind: "assistant-message",
      content: { text: "second message" },
      sourceRef: "fixture",
      observedAt: "2026-11-01T09:00:02.000Z",
    },
  ],
  messageCount: 2,
  runCount: 1,
});

const capability = (
  name: CapabilityName,
  status: Capability["status"],
  evidence: Capability["evidenceLevel"],
): Capability => ({
  agentId: "a-1",
  installationId: null,
  name,
  status,
  versionRange: "0.0.0-fixture",
  evidenceLevel: evidence,
  source: "synthetic fixture",
  verifiedAt: null,
  reason: status === "unverified" ? "fixture only; live NOT_RUN" : null,
});

const page = <T,>(items: T[], nextCursor: string | null = null): Page<T> => ({
  items,
  nextCursor,
  total: items.length,
});

class FakeApi implements MultiCliApi {
  workspaces: Page<WorkspaceListItem> = page([]);
  agents: Page<AgentListItem> = page([]);
  sessions: Page<SessionSummary> = page([]);
  details: Record<string, SessionDetail> = {};
  capabilities: Page<Capability> = page([]);
  capabilitiesNext: Page<Capability> | null = null;
  capabilityMoreFailures = 0;
  sessionCalls: SessionQuery[] = [];
  workspaceFailures = 0;
  sessionsNext: Page<SessionSummary> | null = null;
  sessionMoreFailures = 0;
  sessionRefreshFailures = 0;
  sessionFailures = 0;
  detailFailures = 0;
  exportResult: DiagnosticsExport = {
    path: "C:\\diagnostics\\diagnostics-1.json",
    createdAt: "2026-11-01T09:00:00.000Z",
  };
  exportFailure: unknown = null;
  listSessionsOverride:
    | ((query: SessionQuery) => Promise<Page<SessionSummary>>)
    | null = null;

  listWorkspaces(): Promise<Page<WorkspaceListItem>> {
    if (this.workspaceFailures > 0) {
      this.workspaceFailures -= 1;
      return Promise.reject({
        code: "database",
        message: "database is unavailable",
        retryable: true,
        details: { stage: "open" },
      });
    }
    return Promise.resolve(this.workspaces);
  }

  listAgents(): Promise<Page<AgentListItem>> {
    return Promise.resolve(this.agents);
  }

  listSessions(query: SessionQuery): Promise<Page<SessionSummary>> {
    this.sessionCalls.push(query);
    if (this.sessionFailures > 0 && !query.cursor) {
      this.sessionFailures -= 1;
      return Promise.reject({
        code: "database",
        message: "session list unavailable",
        retryable: true,
        details: { stage: "listSessions" },
      });
    }
    if (this.listSessionsOverride) {
      return this.listSessionsOverride(query);
    }
    if (query.cursor) {
      if (this.sessionMoreFailures > 0) {
        this.sessionMoreFailures -= 1;
        return Promise.reject({
          code: "database",
          message: "next page unavailable",
          retryable: true,
          details: { stage: "listSessions" },
        });
      }
      return Promise.resolve(this.sessionsNext ?? page([]));
    }
    if (this.sessionCalls.length > 1 && this.sessionRefreshFailures > 0) {
      this.sessionRefreshFailures -= 1;
      return Promise.reject({
        code: "database",
        message: "refresh failed",
        retryable: true,
        details: { stage: "listSessions" },
      });
    }
    return Promise.resolve(this.sessions);
  }

  getSession(sessionId: string): Promise<SessionDetail> {
    if (this.detailFailures > 0) {
      this.detailFailures -= 1;
      return Promise.reject({
        code: "database",
        message: "detail unavailable",
        retryable: true,
        details: { stage: "getSession" },
      });
    }
    const value = this.details[sessionId];
    if (!value) {
      return Promise.reject({
        code: "not_found",
        message: "session not found",
        retryable: false,
        details: { entity: "session", id: sessionId },
      });
    }
    return Promise.resolve(value);
  }

  listCapabilities(query: CapabilityQuery): Promise<Page<Capability>> {
    if (query.cursor) {
      if (this.capabilityMoreFailures > 0) {
        this.capabilityMoreFailures -= 1;
        return Promise.reject({
          code: "database",
          message: "capability page unavailable",
          retryable: true,
          details: { stage: "listCapabilities" },
        });
      }
      return Promise.resolve(this.capabilitiesNext ?? page([]));
    }
    return Promise.resolve(this.capabilities);
  }

  exportDiagnostics(): Promise<DiagnosticsExport> {
    if (this.exportFailure) {
      return Promise.reject(this.exportFailure);
    }
    return Promise.resolve(this.exportResult);
  }
}

async function flush(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

interface Rendered {
  container: HTMLElement;
  root: Root;
  cleanup: () => Promise<void>;
}

async function renderApp(api: MultiCliApi): Promise<Rendered> {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(async () => {
    root.render(<App api={api} />);
  });
  await flush();
  return {
    container,
    root,
    cleanup: async () => {
      await act(async () => {
        root.unmount();
      });
      container.remove();
    },
  };
}

function byTestId<T extends Element = HTMLElement>(container: HTMLElement, id: string): T | null {
  return container.querySelector<T>(`[data-testid="${id}"]`);
}

async function click(element: Element | null): Promise<void> {
  if (!element) {
    throw new Error("clicked element is missing");
  }
  await act(async () => {
    element.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
}

let rendered: Rendered | null = null;

afterEach(async () => {
  if (rendered) {
    await rendered.cleanup();
    rendered = null;
  }
});

describe("S01 navigation app", () => {
  it("shows distinct empty states and never fabricates demo rows", async () => {
    const api = new FakeApi();
    rendered = await renderApp(api);
    expect(byTestId(rendered.container, "empty-workspaces")).not.toBeNull();
    expect(byTestId(rendered.container, "empty-sessions")).not.toBeNull();
    expect(byTestId(rendered.container, "detail-empty")).not.toBeNull();
    expect(
      rendered.container.querySelectorAll('[data-testid^="session-row-"]').length,
    ).toBe(0);
  });

  it("shows a real AppError and retries without turning it into an empty success", async () => {
    const api = new FakeApi();
    api.workspaceFailures = 1;
    rendered = await renderApp(api);
    const error = byTestId(rendered.container, "workspaces-list-error");
    expect(error).not.toBeNull();
    expect(byTestId(rendered.container, "workspaces-list-error-code")?.textContent).toBe(
      "database",
    );
    expect(byTestId(rendered.container, "empty-workspaces")).toBeNull();
    await click(byTestId(rendered.container, "workspaces-list-retry"));
    expect(byTestId(rendered.container, "workspaces-list-error")).toBeNull();
    expect(byTestId(rendered.container, "empty-workspaces")).not.toBeNull();
  });

  it("opens the same session from workspace and agent entries with identical counts", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha 项目")]);
    api.agents = page([agent("a-1", "Codex")]);
    api.sessions = page([summary("s-1", "a-1", "跨工作区会话")]);
    api.details["s-1"] = detail("s-1", "a-1", "跨工作区会话");
    api.capabilities = page([
      capability("structuredChat", "supported", "live"),
      capability("permissionFlow", "unverified", "fixture"),
    ]);
    rendered = await renderApp(api);

    await click(byTestId(rendered.container, "workspace-row-ws-1"));
    expect(api.sessionCalls.at(-1)?.workspaceId).toBe("ws-1");
    expect(byTestId(rendered.container, "session-row-s-1")).not.toBeNull();
    await click(byTestId(rendered.container, "session-row-s-1"));
    expect(byTestId(rendered.container, "session-detail")).not.toBeNull();
    expect(byTestId(rendered.container, "detail-session-id")?.textContent).toBe("s-1");
    expect(byTestId(rendered.container, "detail-count-messages")?.textContent).toBe("2");
    expect(byTestId(rendered.container, "detail-count-runs")?.textContent).toBe("1");
    expect(byTestId(rendered.container, "capability-row-structuredChat")).not.toBeNull();

    await click(byTestId(rendered.container, "entry-agents"));
    await click(byTestId(rendered.container, "agent-row-a-1"));
    expect(api.sessionCalls.at(-1)?.agentId).toBe("a-1");
    expect(api.sessionCalls.at(-1)?.workspaceId).toBeNull();
    await click(byTestId(rendered.container, "session-row-s-1"));
    expect(byTestId(rendered.container, "detail-session-id")?.textContent).toBe("s-1");
    expect(byTestId(rendered.container, "detail-count-messages")?.textContent).toBe("2");
    expect(byTestId(rendered.container, "detail-count-runs")?.textContent).toBe("1");
  });

  it("resets the cursor when a filter changes", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    api.sessions = page([summary("s-1", "a-1", "Session")], "cursor-1");
    rendered = await renderApp(api);
    expect(api.sessionCalls[0].cursor).toBeNull();

    const select = byTestId<HTMLSelectElement>(rendered.container, "origin-filter");
    await act(async () => {
      if (!select) throw new Error("origin filter missing");
      select.value = "managed";
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await flush();
    expect(api.sessionCalls.at(-1)?.origin).toBe("managed");
    expect(api.sessionCalls.at(-1)?.cursor).toBeNull();
  });

  it("shows a loading state before the first session page resolves", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    let resolveSessions: ((value: Page<SessionSummary>) => void) | null = null;
    api.listSessionsOverride = () =>
      new Promise<Page<SessionSummary>>((resolve) => {
        resolveSessions = resolve;
      });
    rendered = await renderApp(api);
    expect(byTestId(rendered.container, "sessions-loading")).not.toBeNull();
    expect(byTestId(rendered.container, "session-row-s-1")).toBeNull();
    await act(async () => {
      resolveSessions?.(page([summary("s-1", "a-1", "Session")]));
    });
    await flush();
    expect(byTestId(rendered.container, "sessions-loading")).toBeNull();
    expect(byTestId(rendered.container, "session-row-s-1")).not.toBeNull();
  });

  it("retries a failed load-more page without dropping loaded rows", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    api.sessions = page([summary("s-1", "a-1", "First")], "cursor-1");
    api.sessionsNext = page([summary("s-2", "a-1", "Second")]);
    api.sessionMoreFailures = 1;
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "sessions-load-more"));
    expect(byTestId(rendered.container, "sessions-more-error")).not.toBeNull();
    expect(byTestId(rendered.container, "sessions-more-error-code")?.textContent).toBe(
      "database",
    );
    expect(byTestId(rendered.container, "session-row-s-1")).not.toBeNull();
    await click(byTestId(rendered.container, "sessions-more-retry"));
    expect(byTestId(rendered.container, "sessions-more-error")).toBeNull();
    expect(byTestId(rendered.container, "session-row-s-1")).not.toBeNull();
    expect(byTestId(rendered.container, "session-row-s-2")).not.toBeNull();
  });

  it("retries a failed capability page instead of hiding it", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    api.sessions = page([summary("s-1", "a-1", "Session")]);
    api.details["s-1"] = detail("s-1", "a-1", "Session");
    api.capabilities = page(
      [capability("structuredChat", "supported", "live")],
      "cap-1",
    );
    api.capabilitiesNext = page([
      capability("permissionFlow", "unverified", "fixture"),
    ]);
    api.capabilityMoreFailures = 1;
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "workspace-row-ws-1"));
    await click(byTestId(rendered.container, "session-row-s-1"));
    await click(byTestId(rendered.container, "capabilities-load-more"));
    expect(byTestId(rendered.container, "capabilities-more-error")).not.toBeNull();
    expect(byTestId(rendered.container, "capabilities-more-error-code")?.textContent).toBe(
      "database",
    );
    expect(byTestId(rendered.container, "capability-row-structuredChat")).not.toBeNull();
    await click(byTestId(rendered.container, "capabilities-more-retry"));
    expect(byTestId(rendered.container, "capabilities-more-error")).toBeNull();
    expect(byTestId(rendered.container, "capability-row-permissionFlow")).not.toBeNull();
  });

  it("ignores a late session page after the filter changed", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    const resolvers: Array<(value: Page<SessionSummary>) => void> = [];
    let calls = 0;
    api.listSessionsOverride = () => {
      calls += 1;
      return new Promise<Page<SessionSummary>>((resolve) => {
        resolvers.push(resolve);
      });
    };
    rendered = await renderApp(api);
    expect(calls).toBe(1);

    await click(byTestId(rendered.container, "workspace-row-ws-1"));
    expect(calls).toBe(2);
    await act(async () => {
      resolvers[1](page([summary("s-new", "a-1", "New")]));
    });
    await flush();
    expect(byTestId(rendered.container, "session-row-s-new")).not.toBeNull();

    await act(async () => {
      resolvers[0](page([summary("s-stale", "a-1", "Stale")]));
    });
    await flush();
    expect(byTestId(rendered.container, "session-row-s-stale")).toBeNull();
    expect(byTestId(rendered.container, "session-row-s-new")).not.toBeNull();
  });

  it("keeps existing rows when a refresh fails, then recovers", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    api.sessions = page([summary("s-1", "a-1", "Session")]);
    rendered = await renderApp(api);
    expect(byTestId(rendered.container, "session-row-s-1")).not.toBeNull();

    api.sessionRefreshFailures = 1;
    await click(byTestId(rendered.container, "sessions-refresh"));
    expect(byTestId(rendered.container, "sessions-error")).not.toBeNull();
    expect(byTestId(rendered.container, "session-row-s-1")).not.toBeNull();
    await click(byTestId(rendered.container, "sessions-retry"));
    expect(byTestId(rendered.container, "sessions-error")).toBeNull();
    expect(byTestId(rendered.container, "session-row-s-1")).not.toBeNull();
  });

  it("shows and retries a session-list error", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    api.sessions = page([summary("s-1", "a-1", "Session")]);
    api.sessionFailures = 1;
    rendered = await renderApp(api);
    expect(byTestId(rendered.container, "sessions-error")).not.toBeNull();
    expect(byTestId(rendered.container, "sessions-error-code")?.textContent).toBe("database");
    await click(byTestId(rendered.container, "sessions-retry"));
    expect(byTestId(rendered.container, "sessions-error")).toBeNull();
    expect(byTestId(rendered.container, "session-row-s-1")).not.toBeNull();
  });

  it("shows and retries a detail error that is not not-found", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    api.sessions = page([summary("s-1", "a-1", "Session")]);
    api.details["s-1"] = detail("s-1", "a-1", "Session");
    api.detailFailures = 1;
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "workspace-row-ws-1"));
    await click(byTestId(rendered.container, "session-row-s-1"));
    expect(byTestId(rendered.container, "detail-error")).not.toBeNull();
    expect(byTestId(rendered.container, "detail-error-code")?.textContent).toBe("database");
    expect(byTestId(rendered.container, "detail-not-found")).toBeNull();
    await click(byTestId(rendered.container, "detail-retry"));
    expect(byTestId(rendered.container, "detail-error")).toBeNull();
    expect(byTestId(rendered.container, "detail-session-id")?.textContent).toBe("s-1");
  });

  it("renders a missing session distinctly from a generic error", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    api.sessions = page([summary("s-1", "a-1", "Session")]);
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "workspace-row-ws-1"));
    await click(byTestId(rendered.container, "session-row-s-1"));
    expect(byTestId(rendered.container, "detail-not-found")).not.toBeNull();
    expect(byTestId(rendered.container, "detail-error")).toBeNull();
  });

  it("shows the real diagnostics path or a real export error", async () => {
    const api = new FakeApi();
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "export-button"));
    expect(byTestId(rendered.container, "export-result")?.textContent).toContain(
      api.exportResult.path,
    );

    api.exportFailure = {
      code: "io",
      message: "diagnostics directory could not be created",
      retryable: true,
      details: { stage: "exportDiagnostics" },
    };
    await click(byTestId(rendered.container, "export-button"));
    expect(byTestId(rendered.container, "export-error")).not.toBeNull();
    expect(byTestId(rendered.container, "export-error-code")?.textContent).toBe("io");
    expect(byTestId(rendered.container, "export-result")).toBeNull();
  });
});
