/**
 * Typed IPC client for the six S01 product commands. Every rejection is
 * surfaced as a real AppError; failures are never converted into empty pages.
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  AgentListItem,
  Capability,
  CapabilityQuery,
  DiagnosticsExport,
  Page,
  PageQuery,
  SessionDetail,
  SessionQuery,
  SessionSummary,
  WorkspaceListItem,
} from "./types";

export type InvokeFn = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export interface MultiCliApi {
  listWorkspaces(query: PageQuery): Promise<Page<WorkspaceListItem>>;
  listAgents(query: PageQuery): Promise<Page<AgentListItem>>;
  listSessions(query: SessionQuery): Promise<Page<SessionSummary>>;
  getSession(sessionId: string): Promise<SessionDetail>;
  listCapabilities(query: CapabilityQuery): Promise<Page<Capability>>;
  exportDiagnostics(): Promise<DiagnosticsExport>;
}

function pageArgs(query: PageQuery): Record<string, unknown> {
  const payload: Record<string, unknown> = { limit: query.limit };
  if (query.cursor) {
    payload.cursor = query.cursor;
  }
  return payload;
}

function sessionArgs(query: SessionQuery): Record<string, unknown> {
  const payload = pageArgs(query);
  if (query.workspaceId) {
    payload.workspaceId = query.workspaceId;
  }
  if (query.agentId) {
    payload.agentId = query.agentId;
  }
  if (query.origin) {
    payload.origin = query.origin;
  }
  return payload;
}

export function createMultiCliApi(invokeFn: InvokeFn): MultiCliApi {
  return {
    listWorkspaces: (query) =>
      invokeFn<Page<WorkspaceListItem>>("multicli_list_workspaces", {
        query: pageArgs(query),
      }),
    listAgents: (query) =>
      invokeFn<Page<AgentListItem>>("multicli_list_agents", {
        query: pageArgs(query),
      }),
    listSessions: (query) =>
      invokeFn<Page<SessionSummary>>("multicli_list_sessions", {
        query: sessionArgs(query),
      }),
    getSession: (sessionId) =>
      invokeFn<SessionDetail>("multicli_get_session", { sessionId }),
    listCapabilities: (query) =>
      invokeFn<Page<Capability>>("multicli_list_capabilities", {
        query: { agentId: query.agentId, ...pageArgs(query) },
      }),
    exportDiagnostics: () =>
      invokeFn<DiagnosticsExport>("multicli_export_diagnostics"),
  };
}

export const multiCliApi: MultiCliApi = createMultiCliApi((command, args) =>
  invoke(command, args),
);
