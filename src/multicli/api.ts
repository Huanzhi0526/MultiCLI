/**
 * Typed IPC client for the six S01 product commands. Every rejection is
 * surfaced as a real AppError; failures are never converted into empty pages.
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  AgentListItem,
  Capability,
  CapabilityQuery,
  CustomCliInput,
  CustomCliView,
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
  RuntimeAnswerInput,
  RuntimeCapabilitiesView,
  RuntimeEventPage,
  RuntimeListView,
  RuntimeReadQuery,
  RuntimeResumeInput,
  RuntimeRunView,
  RuntimeStartInput,
  ScanView,
  SearchPage,
  SessionDetail,
  SessionMetadataView,
  SessionQuery,
  SessionSummary,
  TerminalCloseView,
  UsageOverviewView,
  UsageRefreshResult,
  TerminalLimits,
  TerminalListView,
  TerminalOpenSpec,
  TerminalQuitCheckView,
  TerminalReadQuery,
  TerminalReadView,
  TerminalResizeView,
  TerminalShutdownAllView,
  TerminalStopMode,
  TerminalStopView,
  TerminalTargetsView,
  TerminalView,
  TerminalWriteView,
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
  // S04 managed terminal
  terminalTargets(): Promise<TerminalTargetsView>;
  terminalOpen(spec: TerminalOpenSpec): Promise<TerminalView>;
  terminalList(): Promise<TerminalListView>;
  terminalRead(query: TerminalReadQuery): Promise<TerminalReadView>;
  terminalWrite(terminalId: string, data: string): Promise<TerminalWriteView>;
  terminalResize(terminalId: string, cols: number, rows: number): Promise<TerminalResizeView>;
  terminalStop(
    terminalId: string,
    mode: TerminalStopMode,
    graceMs?: number,
  ): Promise<TerminalStopView>;
  terminalClose(
    terminalId: string,
    mode?: TerminalStopMode,
    graceMs?: number,
  ): Promise<TerminalCloseView>;
  terminalShutdownAll(): Promise<TerminalShutdownAllView>;
  terminalLimits(): Promise<TerminalLimits>;
  terminalQuitCheck(): Promise<TerminalQuitCheckView>;
  appQuit(): Promise<void>;
  // S05 structured runtime
  runtimeCapabilities(): Promise<RuntimeCapabilitiesView>;
  runtimeStart(input: RuntimeStartInput): Promise<RuntimeRunView>;
  runtimeResume(input: RuntimeResumeInput): Promise<RuntimeRunView>;
  runtimeSend(runId: string, text: string): Promise<RuntimeRunView>;
  runtimeCancel(runId: string, graceMs?: number): Promise<RuntimeRunView>;
  runtimeAnswer(input: RuntimeAnswerInput): Promise<RuntimeRunView>;
  runtimeStop(runId: string, graceMs?: number): Promise<RuntimeRunView>;
  runtimeGet(runId: string): Promise<RuntimeRunView>;
  runtimeRead(query: RuntimeReadQuery): Promise<RuntimeEventPage>;
  runtimeList(): Promise<RuntimeListView>;
  // S06 usage centre and custom CLIs
  usageOverview(): Promise<UsageOverviewView>;
  usageRefresh(agentId?: string | null, force?: boolean): Promise<UsageRefreshResult>;
  customCliList(): Promise<CustomCliView[]>;
  customCliCreate(input: CustomCliInput): Promise<CustomCliView>;
  customCliDelete(id: string): Promise<void>;
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
    terminalTargets: () => invokeFn<TerminalTargetsView>("multicli_terminal_targets"),
    terminalOpen: (spec) => invokeFn<TerminalView>("multicli_terminal_open", { spec }),
    terminalList: () => invokeFn<TerminalListView>("multicli_terminal_list"),
    terminalRead: (query) =>
      invokeFn<TerminalReadView>("multicli_terminal_read", {
        query: {
          terminalId: query.terminalId,
          ...(query.cursor === undefined || query.cursor === null
            ? {}
            : { cursor: query.cursor }),
          ...(query.maxBytes ? { maxBytes: query.maxBytes } : {}),
        },
      }),
    terminalWrite: (terminalId, data) =>
      invokeFn<TerminalWriteView>("multicli_terminal_write", {
        query: { terminalId, data },
      }),
    terminalResize: (terminalId, cols, rows) =>
      invokeFn<TerminalResizeView>("multicli_terminal_resize", {
        query: { terminalId, cols, rows },
      }),
    terminalStop: (terminalId, mode, graceMs) =>
      invokeFn<TerminalStopView>("multicli_terminal_stop", {
        query: {
          terminalId,
          mode,
          ...(graceMs ? { graceMs } : {}),
        },
      }),
    terminalClose: (terminalId, mode, graceMs) =>
      invokeFn<TerminalCloseView>("multicli_terminal_close", {
        query: {
          terminalId,
          ...(mode ? { mode } : {}),
          ...(graceMs ? { graceMs } : {}),
        },
      }),
    terminalShutdownAll: () =>
      invokeFn<TerminalShutdownAllView>("multicli_terminal_shutdown_all"),
    terminalLimits: () => invokeFn<TerminalLimits>("multicli_terminal_limits"),
    terminalQuitCheck: () =>
      invokeFn<TerminalQuitCheckView>("multicli_terminal_quit_check"),
    appQuit: () => invokeFn<void>("multicli_app_quit", { confirm: true }),
    runtimeCapabilities: () =>
      invokeFn<RuntimeCapabilitiesView>("multicli_runtime_capabilities"),
    runtimeStart: (input) =>
      invokeFn<RuntimeRunView>("multicli_runtime_start", {
        spec: {
          installationId: input.installationId,
          cwd: input.cwd,
          ...(input.prompt ? { prompt: input.prompt } : {}),
          ...(input.title ? { title: input.title } : {}),
          ...(input.permissionTimeoutMs
            ? { permissionTimeoutMs: input.permissionTimeoutMs }
            : {}),
          ...(input.model ? { model: input.model } : {}),
          ...(input.effort ? { effort: input.effort } : {}),
        },
      }),
    runtimeResume: (input) =>
      invokeFn<RuntimeRunView>("multicli_runtime_resume", {
        spec: {
          sessionId: input.sessionId,
          ...(input.installationId ? { installationId: input.installationId } : {}),
          ...(input.cwd ? { cwd: input.cwd } : {}),
          ...(input.prompt ? { prompt: input.prompt } : {}),
          ...(input.permissionTimeoutMs
            ? { permissionTimeoutMs: input.permissionTimeoutMs }
            : {}),
        },
      }),
    runtimeSend: (runId, text) =>
      invokeFn<RuntimeRunView>("multicli_runtime_send", { query: { runId, text } }),
    runtimeCancel: (runId, graceMs) =>
      invokeFn<RuntimeRunView>("multicli_runtime_cancel", {
        query: { runId, ...(graceMs ? { graceMs } : {}) },
      }),
    runtimeAnswer: (input) =>
      invokeFn<RuntimeRunView>("multicli_runtime_answer", {
        query: {
          runId: input.runId,
          permissionId: input.permissionId,
          optionId: input.optionId,
        },
      }),
    runtimeStop: (runId, graceMs) =>
      invokeFn<RuntimeRunView>("multicli_runtime_stop", {
        query: { runId, ...(graceMs ? { graceMs } : {}) },
      }),
    runtimeGet: (runId) => invokeFn<RuntimeRunView>("multicli_runtime_get", { runId }),
    runtimeRead: (query) =>
      invokeFn<RuntimeEventPage>("multicli_runtime_read", {
        query: {
          runId: query.runId,
          ...(query.cursor === undefined || query.cursor === null
            ? {}
            : { cursor: query.cursor }),
          ...(query.maxEvents ? { maxEvents: query.maxEvents } : {}),
        },
      }),
    runtimeList: () => invokeFn<RuntimeListView>("multicli_runtime_list"),
    usageOverview: () => invokeFn<UsageOverviewView>("multicli_usage_overview"),
    usageRefresh: (agentId, force) =>
      invokeFn<UsageRefreshResult>("multicli_usage_refresh", {
        query: {
          ...(agentId ? { agentId } : {}),
          ...(force ? { force: true } : {}),
        },
      }),
    customCliList: () => invokeFn<CustomCliView[]>("multicli_custom_cli_list"),
    customCliCreate: (input) =>
      invokeFn<CustomCliView>("multicli_custom_cli_create", {
        query: {
          label: input.label,
          executable: input.executable,
          args: input.args,
          ...(input.cwd ? { cwd: input.cwd } : {}),
          ...(input.productHint ? { productHint: input.productHint } : {}),
        },
      }),
    customCliDelete: (id) =>
      invokeFn<void>("multicli_custom_cli_delete", { query: { id } }),
  };
}

export const multiCliApi: MultiCliApi = createMultiCliApi((command, args) =>
  invoke(command, args),
);
