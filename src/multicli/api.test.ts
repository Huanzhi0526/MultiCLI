import { describe, expect, it } from "vitest";
import { createMultiCliApi } from "./api";
import { toAppError } from "./navigation";
import type { Page, WorkspaceListItem } from "./types";

const emptyPage: Page<WorkspaceListItem> = { items: [], nextCursor: null, total: 0 };

describe("typed IPC client", () => {
  it("sends camelCase query envelopes and omits empty filters", async () => {
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const api = createMultiCliApi(async <T,>(command: string, args?: Record<string, unknown>) => {
      calls.push({ command, args });
      return emptyPage as T;
    });

    await api.listWorkspaces({ limit: 50 });
    await api.listSessions({
      limit: 20,
      cursor: "abc",
      workspaceId: "w-1",
      agentId: null,
      origin: null,
    });
    await api.listCapabilities({ agentId: "a-1", limit: 200 });
    await api.getSession("s-1");
    await api.exportDiagnostics();

    expect(calls[0]).toEqual({
      command: "multicli_list_workspaces",
      args: { query: { limit: 50 } },
    });
    expect(calls[1]).toEqual({
      command: "multicli_list_sessions",
      args: {
        query: { limit: 20, cursor: "abc", workspaceId: "w-1" },
      },
    });
    expect(calls[2]).toEqual({
      command: "multicli_list_capabilities",
      args: { query: { agentId: "a-1", limit: 200 } },
    });
    expect(calls[3]).toEqual({
      command: "multicli_get_session",
      args: { sessionId: "s-1" },
    });
    expect(calls[4]).toEqual({ command: "multicli_export_diagnostics", args: undefined });
  });

  it("propagates AppError instead of returning an empty page", async () => {
    const api = createMultiCliApi(async () => {
      throw { code: "database", message: "db down", retryable: true, details: {} };
    });
    let result: unknown = null;
    try {
      await api.listWorkspaces({ limit: 50 });
    } catch (error) {
      result = error;
    }
    expect(result).not.toBeNull();
    expect(toAppError(result)).toEqual({
      code: "database",
      message: "db down",
      retryable: true,
      details: {},
    });
  });
});
