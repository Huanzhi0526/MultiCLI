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
  DiscoverySettings,
  HistoryCandidateView,
  HistoryOverallStatus,
  HistorySearchQuery,
  HistorySourceUpdateInput,
  HistorySourceView,
  InstallationDiscoveryView,
  InstallationQuery,
  MessagePage,
  MessagePageQuery,
  Page,
  PageQuery,
  ProcessDiscoveryPage,
  ProcessQuery,
  ResumeTargetView,
  ScanView,
  SearchPage,
  SessionDetail,
  SessionMetadataView,
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
  listInstallations(query: InstallationQuery): Promise<Page<InstallationDiscoveryView>>;
  listProcesses(query: ProcessQuery): Promise<ProcessDiscoveryPage>;
  refreshDiscovery(): Promise<ScanView>;
  updateDiscoverySettings(intervalMs: number): Promise<DiscoverySettings>;
  listHistorySources(query: PageQuery): Promise<Page<HistorySourceView>>;
  listHistoryCandidates(): Promise<HistoryCandidateView[]>;
  updateHistorySource(input: HistorySourceUpdateInput): Promise<HistorySourceView>;
  refreshHistory(sourceId?: string | null): Promise<HistoryOverallStatus>;
  getHistoryStatus(): Promise<HistoryOverallStatus>;
  rebuildHistoryIndex(sourceId: string): Promise<HistoryOverallStatus>;
  getSessionMetadata(sessionId: string): Promise<SessionMetadataView>;
  listSessionMessages(query: MessagePageQuery): Promise<MessagePage>;
  searchHistory(query: HistorySearchQuery): Promise<SearchPage>;
  resolveResumeTarget(sessionId: string): Promise<ResumeTargetView>;
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
    listInstallations: (query) =>
      invokeFn<Page<InstallationDiscoveryView>>("multicli_list_installations", {
        query: {
          ...(query.agentId ? { agentId: query.agentId } : {}),
          ...pageArgs(query),
        },
      }),
    listProcesses: (query) =>
      invokeFn<ProcessDiscoveryPage>("multicli_list_processes", {
        query: {
          ...(query.agentId ? { agentId: query.agentId } : {}),
          ...(query.workspaceId ? { workspaceId: query.workspaceId } : {}),
          ...(query.state ? { state: query.state } : {}),
          ...pageArgs(query),
        },
      }),
    refreshDiscovery: () => invokeFn<ScanView>("multicli_refresh_discovery"),
    updateDiscoverySettings: (intervalMs) =>
      invokeFn<DiscoverySettings>("multicli_update_discovery_settings", {
        query: { intervalMs },
      }),
    listHistorySources: (query) =>
      invokeFn<Page<HistorySourceView>>("multicli_list_history_sources", {
        query: pageArgs(query),
      }),
    listHistoryCandidates: () =>
      invokeFn<HistoryCandidateView[]>("multicli_list_history_candidates"),
    updateHistorySource: (input) =>
      invokeFn<HistorySourceView>("multicli_update_history_source", {
        query: {
          ...(input.sourceId ? { sourceId: input.sourceId } : {}),
          product: input.product,
          ...(input.installationId ? { installationId: input.installationId } : {}),
          configProfileId: input.configProfileId,
          root: input.root,
          enabled: input.enabled,
        },
      }),
    refreshHistory: (sourceId) =>
      invokeFn<HistoryOverallStatus>("multicli_refresh_history", {
        query: sourceId ? { sourceId } : {},
      }),
    getHistoryStatus: () =>
      invokeFn<HistoryOverallStatus>("multicli_get_history_status"),
    rebuildHistoryIndex: (sourceId) =>
      invokeFn<HistoryOverallStatus>("multicli_rebuild_history_index", {
        query: { sourceId },
      }),
    getSessionMetadata: (sessionId) =>
      invokeFn<SessionMetadataView>("multicli_get_session_metadata", { sessionId }),
    listSessionMessages: (query) =>
      invokeFn<MessagePage>("multicli_list_session_messages", {
        query: {
          sessionId: query.sessionId,
          limit: query.limit,
          ...(query.cursor ? { cursor: query.cursor } : {}),
          ...(query.anchorMessageId
            ? { anchorMessageId: query.anchorMessageId }
            : {}),
        },
      }),
    searchHistory: (query) =>
      invokeFn<SearchPage>("multicli_search_history", {
        query: {
          text: query.text,
          ...(query.workspaceId ? { workspaceId: query.workspaceId } : {}),
          ...(query.agentId ? { agentId: query.agentId } : {}),
          ...(query.sourceId ? { sourceId: query.sourceId } : {}),
          limit: query.limit,
          ...(query.cursor ? { cursor: query.cursor } : {}),
        },
      }),
    resolveResumeTarget: (sessionId) =>
      invokeFn<ResumeTargetView>("multicli_resolve_resume_target", { sessionId }),
  };
}

export const multiCliApi: MultiCliApi = createMultiCliApi((command, args) =>
  invoke(command, args),
);
