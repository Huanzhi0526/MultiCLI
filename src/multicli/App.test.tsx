// @vitest-environment jsdom
import { act, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import App from "./App";
import type { MultiCliApi } from "./api";
import type {
  AgentListItem,
  BackupRecord,
  Capability,
  CapabilityName,
  CapabilityQuery,
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
  Message,
  MessagePage,
  MessagePageQuery,
  Page,
  PendingRestore,
  ProcessDiscoveryPage,
  ProcessDiscoveryView,
  ProcessQuery,
  ProfileDataView,
  ResumeTargetView,
  RuntimeCapabilitiesView,
  RuntimeEventPage,
  RuntimeListView,
  RuntimeRunView,
  ScanView,
  SearchHitView,
  SearchPage,
  SessionDetail,
  SessionMetadataView,
  SessionQuery,
  SessionSummary,
  TerminalCloseView,
  TerminalLimits,
  TerminalListView,
  TerminalQuitCheckView,
  TerminalReadView,
  TerminalResizeView,
  TerminalShutdownAllView,
  TerminalStopView,
  TerminalTargetsView,
  TerminalView,
  TerminalWriteView,
  UsageOverviewView,
  UsageRefreshResult,
  WorkspaceListItem,
} from "./types";

const TERMINAL_LIMITS: TerminalLimits = {
  bufferBytes: 4 * 1024 * 1024,
  minBufferBytes: 256 * 1024,
  maxBufferBytes: 64 * 1024 * 1024,
  logBytes: 20 * 1024 * 1024,
  minLogBytes: 1024 * 1024,
  maxLogBytes: 512 * 1024 * 1024,
  maxTerminals: 16,
  note: "synthetic limits",
};

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

const idleScan = (overrides: Partial<ScanView> = {}): ScanView => ({
  generation: 0,
  status: "idle",
  inFlight: false,
  lastAttemptAt: null,
  lastSuccessAt: null,
  error: null,
  installError: null,
  intervalMs: 3000,
  scope: "local-windows",
  sampledProcesses: 0,
  unreadableProcesses: 0,
  trackedProcesses: 0,
  note: "Sampled periodically; processes shorter than the scan interval can be missed. Not an audit log.",
  ...overrides,
});

const installationView = (
  overrides: Partial<InstallationDiscoveryView> = {},
): InstallationDiscoveryView => ({
  installationId: "install-1",
  agentId: "codex",
  product: "Codex CLI",
  signatureId: "codex-cli",
  status: "installed",
  entryKind: "native-exe",
  executable: {
    hostId: "local-windows",
    displayPath: "C:\\Tools\\codex.exe",
    canonicalPath: "C:\\Tools\\codex.exe",
  },
  runtime: null,
  entryScript: null,
  version: "0.166.0",
  versionSource: "probe",
  probe: {
    status: "ok",
    reason: null,
    checkedAt: "2026-11-02T01:00:00.000Z",
    durationMs: 120,
    arguments: ["--version"],
  },
  configProfileId: "unconfirmed-local",
  configProfileStatus: "unconfirmed",
  issueCode: null,
  issueReason: null,
  discoveredFrom: "path",
  discoveredAt: "2026-11-02T01:00:00.000Z",
  lastSeenScan: 1,
  ...overrides,
});

const processView = (overrides: Partial<ProcessDiscoveryView> = {}): ProcessDiscoveryView => ({
  identity: {
    hostId: "local-windows",
    pid: 4242,
    creationTime: "2026-11-02T01:00:00.1234567Z",
  },
  pid: 4242,
  agentId: "codex",
  product: "Codex CLI",
  installationId: "install-1",
  entryKind: "native-exe",
  recognition: "confirmed",
  evidence: ["installation-exe"],
  state: "alive",
  freshness: "fresh",
  stateReason: null,
  executable: {
    hostId: "local-windows",
    displayPath: "C:\\Tools\\codex.exe",
    canonicalPath: "C:\\Tools\\codex.exe",
  },
  redactedCommand: "codex.exe --model <value> <2 withheld>",
  parentIdentity: {
    hostId: "local-windows",
    pid: 100,
    creationTime: "2026-11-02T00:59:00.0000001Z",
  },
  parentNote: null,
  sourceProgram: "powershell.exe",
  workspaceId: null,
  workspaceEvidence: null,
  sessionId: null,
  firstSeenAt: "2026-11-02T01:00:01.000Z",
  lastSeenAt: "2026-11-02T01:00:04.000Z",
  lastSeenScan: 1,
  ...overrides,
});


const historyStatus = (
  overrides: Partial<HistorySourceView["status"]> = {},
): HistorySourceView["status"] => ({
  generation: 1,
  status: "ok",
  inFlight: false,
  lastAttemptAt: "2026-11-02T03:00:00.000Z",
  lastSuccessAt: "2026-11-02T03:00:00.000Z",
  error: null,
  filesTotal: 1,
  filesDone: 1,
  filesSkipped: 0,
  filesFailed: 0,
  messagesIndexed: 3,
  bytesRead: 1024,
  complete: true,
  progressPermille: 1000,
  note: "pass complete",
  ...overrides,
});

const historySource = (overrides: Partial<HistorySourceView> = {}): HistorySourceView => ({
  sourceId: "hsrc-1",
  hostId: "local-windows",
  agentId: "codex",
  product: "codex",
  installationId: null,
  configProfileId: "unconfirmed-local",
  configProfileStatus: "unconfirmed",
  root: {
    hostId: "local-windows",
    displayPath: "C:\\Users\\me\\.codex\\sessions",
    canonicalPath: "C:\\Users\\me\\.codex\\sessions",
  },
  enabled: true,
  parserVersion: 1,
  createdAt: "2026-11-02T02:00:00.000Z",
  updatedAt: "2026-11-02T03:00:00.000Z",
  status: historyStatus(),
  ...overrides,
});

const historyMessage = (id: string, seq: number, text: string): Message => ({
  id,
  sessionId: "hist-session-1",
  runId: null,
  nativeEventId: `native-${id}`,
  sourceKey: `codex:response:${id}`,
  seq,
  kind: seq === 0 ? "user-message" : "assistant-message",
  content: { role: seq === 0 ? "user" : "assistant", text, blocks: ["text"] },
  sourceRef: "path:abcdef#L1",
  observedAt: `2026-11-02T03:00:0${seq}.000Z`,
});

const historyMetadata = (): SessionMetadataView => ({
  session: {
    id: "hist-session-1",
    agentId: "codex",
    hostId: "local-windows",
    sourceNamespace: "history:codex:codex:unconfirmed-local",
    nativeId: "native-1",
    origin: "history-only",
    title: "历史会话标题",
  },
  workspaces: [],
  runs: [],
  runTotal: 0,
  messageCount: 3,
  messageKinds: [{ kind: "user-message", count: 1 }],
  firstMessageAt: "2026-11-02T03:00:00.000Z",
  lastMessageAt: "2026-11-02T03:00:02.000Z",
  history: {
    sourceIds: ["hsrc-1"],
    fileCount: 1,
    parserVersions: [1],
    readOnly: true,
  },
  note: "metadata only",
});

const searchHit = (overrides: Partial<SearchHitView> = {}): SearchHitView => ({
  sessionId: "hist-session-1",
  messageId: "hmsg-1",
  sessionTitle: "历史会话标题",
  agentId: "codex",
  kind: "user-message",
  seq: 0,
  observedAt: "2026-11-02T03:00:00.000Z",
  snippet: "…历史记录里的中文搜索…",
  snippetTruncated: true,
  matchMode: "fts",
  sourceIds: ["hsrc-1"],
  sessionMessageCount: 3,
  ...overrides,
});

const resumeTarget = (overrides: Partial<ResumeTargetView> = {}): ResumeTargetView => ({
  sessionId: "hist-session-1",
  agentId: "codex",
  product: "codex",
  nativeId: "native-1",
  sourceNamespace: "history:codex:codex:unconfirmed-local",
  configProfileId: "unconfirmed-local",
  status: "resolved",
  reason: "a native id and a product are known",
  installationId: "install-codex",
  executable: null,
  workspaces: [],
  conflicts: [],
  evidence: [],
  executionAvailable: false,
  note: "This command only resolves a target.",
  ...overrides,
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
  installations: Page<InstallationDiscoveryView> = page([]);
  processes: Page<ProcessDiscoveryView> = page([]);
  scan: ScanView = idleScan();
  quitCheck: TerminalQuitCheckView = {
    activeCount: 0,
    needsConfirmation: false,
    note: "nothing running",
  };
  quitCalls = 0;
  installationFailures = 0;
  processFailures = 0;
  refreshFailure: unknown = null;
  refreshCalls = 0;
  settingsUpdates: number[] = [];
  settingsFailure: unknown = null;
  installationCalls: InstallationQuery[] = [];
  processCalls: ProcessQuery[] = [];
  historySources: Page<HistorySourceView> = page([]);
  historyCandidates: HistoryCandidateView[] = [];
  historyStatusValue: HistoryOverallStatus = {
    schemaVersion: 1,
    scope: "local-windows",
    inFlight: false,
    sources: [],
    note: "status note",
  };
  historySourcesFailure = 0;
  historySearchResult: SearchPage = {
    items: [],
    nextCursor: null,
    total: 0,
    matchMode: "fts",
    note: "search note",
  };
  historySearchFailure = 0;
  historySearches: HistorySearchQuery[] = [];
  historyMessages: MessagePage = {
    items: [historyMessage("hmsg-1", 0, "历史记录里的中文搜索")],
    nextCursor: null,
    total: 1,
    anchorMessageId: null,
    anchorIndex: null,
  };
  historyMetadataValue: SessionMetadataView = historyMetadata();
  historyMessageCalls: MessagePageQuery[] = [];
  historyUpdates: HistorySourceUpdateInput[] = [];
  historyResume: ResumeTargetView = resumeTarget();
  historyResumeFailure = 0;
  historyRefreshes: (string | null | undefined)[] = [];
  historyRebuilds: string[] = [];

  listHistorySources(): Promise<Page<HistorySourceView>> {
    if (this.historySourcesFailure > 0) {
      this.historySourcesFailure -= 1;
      return Promise.reject({
        code: "database",
        message: "history source list unavailable",
        retryable: true,
        details: { stage: "listHistorySources" },
      });
    }
    return Promise.resolve(this.historySources);
  }

  listHistoryCandidates(): Promise<HistoryCandidateView[]> {
    return Promise.resolve(this.historyCandidates);
  }

  updateHistorySource(input: HistorySourceUpdateInput): Promise<HistorySourceView> {
    this.historyUpdates.push(input);
    return Promise.resolve(
      historySource({
        sourceId: input.sourceId ?? "hsrc-new",
        product: input.product,
        configProfileId: input.configProfileId,
        enabled: input.enabled,
      }),
    );
  }

  refreshHistory(sourceId?: string | null): Promise<HistoryOverallStatus> {
    this.historyRefreshes.push(sourceId);
    return Promise.resolve(this.historyStatusValue);
  }

  getHistoryStatus(): Promise<HistoryOverallStatus> {
    return Promise.resolve(this.historyStatusValue);
  }

  rebuildHistoryIndex(sourceId: string): Promise<HistoryOverallStatus> {
    this.historyRebuilds.push(sourceId);
    return Promise.resolve(this.historyStatusValue);
  }

  /**
   * A session opened from any entry is read as metadata + a bounded message
   * page. The fake derives both from the same `details` fixture, so the S01
   * assertions on id/counts still describe one session, and a session that only
   * exists as a history fixture still resolves through `historyMetadataValue`.
   */
  getSessionMetadata(sessionId?: string): Promise<SessionMetadataView> {
    if (this.detailFailures > 0) {
      this.detailFailures -= 1;
      return Promise.reject({
        code: "database",
        message: "detail unavailable",
        retryable: true,
        details: { stage: "getSessionMetadata" },
      });
    }
    const known = sessionId ? this.details[sessionId] : undefined;
    if (known) {
      return Promise.resolve({
        session: known.session,
        workspaces: known.workspaces.map((link) => ({
          workspaceId: link.workspaceId,
          role: link.role,
          evidence: link.evidence,
          path: null,
        })),
        runs: known.runs,
        runTotal: known.runs.length,
        messageCount: known.messages.length,
        messageKinds: [],
        firstMessageAt: null,
        lastMessageAt: null,
        history: null,
        note: "fixture metadata",
      });
    }
    if (sessionId && sessionId === this.historyMetadataValue.session.id) {
      return Promise.resolve(this.historyMetadataValue);
    }
    return Promise.reject({
      code: "not_found",
      message: "session not found",
      retryable: false,
      details: { entity: "session", id: sessionId },
    });
  }

  listSessionMessages(query: MessagePageQuery): Promise<MessagePage> {
    this.historyMessageCalls.push(query);
    const known = this.details[query.sessionId];
    if (known) {
      const start = query.cursor ? Number(query.cursor) : 0;
      const slice = known.messages.slice(start, start + query.limit);
      const next =
        start + query.limit < known.messages.length
          ? String(start + query.limit)
          : null;
      return Promise.resolve({
        items: slice,
        nextCursor: next,
        total: known.messages.length,
        anchorMessageId: query.anchorMessageId ?? null,
        anchorIndex: null,
      });
    }
    return Promise.resolve({
      ...this.historyMessages,
      anchorMessageId: query.anchorMessageId ?? null,
    });
  }

  searchHistory(query: HistorySearchQuery): Promise<SearchPage> {
    this.historySearches.push(query);
    if (this.historySearchFailure > 0) {
      this.historySearchFailure -= 1;
      return Promise.reject({
        code: "database",
        message: "search unavailable",
        retryable: true,
        details: { stage: "searchHistory" },
      });
    }
    return Promise.resolve(this.historySearchResult);
  }

  resolveResumeTarget(): Promise<ResumeTargetView> {
    if (this.historyResumeFailure > 0) {
      this.historyResumeFailure -= 1;
      return Promise.reject({
        code: "database",
        message: "resume resolution unavailable",
        retryable: true,
        details: { stage: "resolveResumeTarget" },
      });
    }
    return Promise.resolve(this.historyResume);
  }

  // S04 terminal surface. The workspace has its own suite; the App-level tests
  // only need the navigation entry to render and a real "nothing running"
  // answer for the tray quit check.
  terminalTargets(): Promise<TerminalTargetsView> {
    return Promise.resolve({
      schemaVersion: 1,
      items: [],
      limits: TERMINAL_LIMITS,
      cwdDefault: "C:\\Work",
      note: "synthetic terminal targets",
    });
  }

  terminalOpen(): Promise<TerminalView> {
    return Promise.reject({
      code: "unsupported",
      message: "terminal open is not part of this app-level fake",
      retryable: false,
      details: { stage: "terminalOpen" },
    });
  }

  terminalList(): Promise<TerminalListView> {
    return Promise.resolve({
      schemaVersion: 1,
      items: [],
      limits: TERMINAL_LIMITS,
      activeCount: 0,
      note: "synthetic terminal list",
    });
  }

  terminalRead(): Promise<TerminalReadView> {
    return Promise.reject({
      code: "not_found",
      message: "no terminal in this fake",
      retryable: false,
      details: { stage: "terminalRead" },
    });
  }

  terminalWrite(): Promise<TerminalWriteView> {
    return Promise.reject({
      code: "not_found",
      message: "no terminal in this fake",
      retryable: false,
      details: { stage: "terminalWrite" },
    });
  }

  terminalResize(): Promise<TerminalResizeView> {
    return Promise.reject({
      code: "not_found",
      message: "no terminal in this fake",
      retryable: false,
      details: { stage: "terminalResize" },
    });
  }

  terminalStop(): Promise<TerminalStopView> {
    return Promise.reject({
      code: "not_found",
      message: "no terminal in this fake",
      retryable: false,
      details: { stage: "terminalStop" },
    });
  }

  terminalClose(): Promise<TerminalCloseView> {
    return Promise.reject({
      code: "not_found",
      message: "no terminal in this fake",
      retryable: false,
      details: { stage: "terminalClose" },
    });
  }

  terminalShutdownAll(): Promise<TerminalShutdownAllView> {
    return Promise.resolve({ stopped: [], remaining: 0, note: "nothing owned" });
  }

  terminalLimits(): Promise<TerminalLimits> {
    return Promise.resolve(TERMINAL_LIMITS);
  }

  terminalQuitCheck(): Promise<TerminalQuitCheckView> {
    return Promise.resolve(this.quitCheck);
  }

  appQuit(): Promise<void> {
    this.quitCalls += 1;
    return Promise.resolve();
  }

  // The S05 runtime surface. The navigation test only needs these to exist: a
  // structured run is never started from the shell-level assertions.
  runtimeCapabilities(): Promise<RuntimeCapabilitiesView> {
    return Promise.resolve({ schemaVersion: 1, items: [], note: "" });
  }

  runtimeStart(): Promise<RuntimeRunView> {
    return Promise.reject({
      code: "unsupported",
      message: "not started in this test",
      retryable: false,
      details: {},
    });
  }

  runtimeResume(): Promise<RuntimeRunView> {
    return Promise.reject({
      code: "unsupported",
      message: "not started in this test",
      retryable: false,
      details: {},
    });
  }

  runtimeSend(): Promise<RuntimeRunView> {
    return Promise.reject({
      code: "unsupported",
      message: "no run",
      retryable: false,
      details: {},
    });
  }

  runtimeCancel(): Promise<RuntimeRunView> {
    return Promise.reject({
      code: "unsupported",
      message: "no run",
      retryable: false,
      details: {},
    });
  }

  runtimeAnswer(): Promise<RuntimeRunView> {
    return Promise.reject({
      code: "unsupported",
      message: "no run",
      retryable: false,
      details: {},
    });
  }

  runtimeStop(): Promise<RuntimeRunView> {
    return Promise.reject({
      code: "unsupported",
      message: "no run",
      retryable: false,
      details: {},
    });
  }

  runtimeGet(): Promise<RuntimeRunView> {
    return Promise.reject({
      code: "not_found",
      message: "no run",
      retryable: false,
      details: {},
    });
  }

  runtimeRead(): Promise<RuntimeEventPage> {
    return Promise.reject({
      code: "not_found",
      message: "no run",
      retryable: false,
      details: {},
    });
  }

  runtimeList(): Promise<RuntimeListView> {
    return Promise.resolve({
      schemaVersion: 1,
      items: [],
      activeCount: 0,
      maxRuns: 8,
      note: "",
    });
  }

  usageOverview(): Promise<UsageOverviewView> {
    return Promise.resolve({
      schemaVersion: 1,
      generatedAt: "2026-11-03T10:00:00.000Z",
      accounts: [],
      unconfirmedPools: [],
      catalog: [],
      catalogScan: {
        scannedRoots: 0,
        entriesExamined: 0,
        unreadableRoots: 0,
        truncated: false,
        conclusive: false,
        scannedAt: "2026-11-03T10:00:00.000Z",
      },
      lastAttemptAt: null,
      lastSuccessAt: null,
      note: "",
    });
  }

  usageRefresh(): Promise<UsageRefreshResult> {
    return this.usageOverview().then((overview) => ({ overview, outcomes: [] }));
  }

  customCliList(): Promise<CustomCliView[]> {
    return Promise.resolve([]);
  }

  customCliCreate(): Promise<CustomCliView> {
    return Promise.reject({
      code: "unsupported",
      message: "not used in this suite",
      retryable: false,
      details: {},
    });
  }

  customCliDelete(): Promise<void> {
    return Promise.resolve();
  }

  profileData(): Promise<ProfileDataView> {
    return Promise.resolve({
      profile: "test",
      dataRoot: "C:\\Users\\test\\AppData\\Local\\MultiCLI\\test",
      databaseFile: "C:\\Users\\test\\AppData\\Local\\MultiCLI\\test\\multicli.db",
      databaseBytes: 8192,
      databaseSchema: 7,
      expectedSchema: 7,
      journalMode: "wal",
      backupDirectory: "C:\\Users\\test\\AppData\\Local\\MultiCLI\\test\\backups",
      backups: [],
      pendingRestore: null,
      lastRestore: null,
      scopeNote: "只包含 MultiCLI 自有 profile 的 SQLite 数据库。",
    });
  }

  profileBackup(): Promise<BackupRecord> {
    return Promise.reject({
      code: "io",
      message: "not used in this suite",
      retryable: true,
      details: {},
    });
  }

  profileRestore(): Promise<PendingRestore> {
    return Promise.reject({
      code: "invalid_argument",
      message: "not used in this suite",
      retryable: false,
      details: {},
    });
  }

  profileCancelRestore(): Promise<void> {
    return Promise.resolve();
  }

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

  listInstallations(query: InstallationQuery): Promise<Page<InstallationDiscoveryView>> {
    this.installationCalls.push(query);
    if (this.installationFailures > 0) {
      this.installationFailures -= 1;
      return Promise.reject({
        code: "database",
        message: "installations unavailable",
        retryable: true,
        details: { stage: "listInstallations" },
      });
    }
    const items = query.agentId
      ? this.installations.items.filter((item) => item.agentId === query.agentId)
      : this.installations.items;
    return Promise.resolve({ ...this.installations, items, total: items.length });
  }

  listProcesses(query: ProcessQuery): Promise<ProcessDiscoveryPage> {
    this.processCalls.push(query);
    if (this.processFailures > 0) {
      this.processFailures -= 1;
      return Promise.reject({
        code: "io",
        message: "process enumeration failed",
        retryable: true,
        details: { stage: "enumerateProcesses" },
      });
    }
    const items = this.processes.items.filter((item) => {
      if (query.agentId && item.agentId !== query.agentId) return false;
      if (query.state && item.state !== query.state) return false;
      return true;
    });
    return Promise.resolve({
      items,
      nextCursor: this.processes.nextCursor,
      total: items.length,
      scan: this.scan,
    });
  }

  refreshDiscovery(): Promise<ScanView> {
    this.refreshCalls += 1;
    if (this.refreshFailure) {
      const failure = this.refreshFailure;
      this.refreshFailure = null;
      return Promise.reject(failure);
    }
    if (this.scan.generation === 0) {
      this.scan = idleScan({
        generation: 1,
        status: "ok",
        lastAttemptAt: "2026-11-02T01:00:00.000Z",
        lastSuccessAt: "2026-11-02T01:00:00.000Z",
        sampledProcesses: 220,
        trackedProcesses: 1,
      });
    }
    return Promise.resolve(this.scan);
  }

  updateDiscoverySettings(intervalMs: number): Promise<DiscoverySettings> {
    this.settingsUpdates.push(intervalMs);
    if (this.settingsFailure) {
      return Promise.reject(this.settingsFailure);
    }
    this.scan = { ...this.scan, intervalMs };
    return Promise.resolve({
      intervalMs,
      minIntervalMs: 1000,
      maxIntervalMs: 60000,
      note: "bounded",
    });
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

async function renderApp(api: MultiCliApi, sessions = true, strict = false): Promise<Rendered> {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(async () => {
    root.render(strict ? <StrictMode><App api={api} /></StrictMode> : <App api={api} />);
  });
  await flush();
  if (sessions) { await click(byTestId(container, "view-sessions")!); await flush(); }
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

/** Rows carry an index because one product can have several installations. */
function byTestIdPrefix<T extends Element = HTMLElement>(
  container: HTMLElement,
  prefix: string,
): T | null {
  return container.querySelector<T>(`[data-testid^="${prefix}"]`);
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

async function chooseWorkspace(container: HTMLElement, id: string): Promise<void> {
  // Folder navigation is deliberately collapsed initially. Open visible levels
  // until the real workspace is reachable, then exercise the original filter.
  for (let level = 0; level < 50; level++) {
    const row = byTestId(container, `workspace-row-${id}`);
    if (row) { await click(row); return; }
    const branches = container.querySelectorAll('[data-testid="workspace-folder-toggle"][aria-expanded="false"]');
    if (branches.length === 0) throw new Error(`workspace ${id} is not reachable`);
    for (const branch of branches) await click(branch);
  }
  throw new Error(`workspace ${id} hierarchy is unexpectedly deep`);
}

let rendered: Rendered | null = null;

afterEach(async () => {
  if (rendered) {
    await rendered.cleanup();
    rendered = null;
  }
});

describe("S01 navigation app", () => {
  it("switches appearance without changing the current session and stores only the product preference", async () => {
    localStorage.removeItem("multicli.theme");
    try {
      const api = new FakeApi();
      rendered = await renderApp(api);
      const first = api.sessions.items[0];
      if (first) await click(byTestId(rendered.container, `session-row-${first.session.id}`));
      const title = byTestId(rendered.container, "detail-title")?.textContent;
      await click(byTestId(rendered.container, "theme-dark"));
      expect(byTestId(rendered.container, "app-root")?.classList.contains("dark")).toBe(true);
      expect(localStorage.getItem("multicli.theme")).toBe("dark");
      expect(byTestId(rendered.container, "detail-title")?.textContent).toBe(title);
      await click(byTestId(rendered.container, "theme-light"));
      expect(byTestId(rendered.container, "app-root")?.classList.contains("dark")).toBe(false);
      expect(localStorage.getItem("multicli.theme")).toBe("light");
    } finally {
      localStorage.removeItem("multicli.theme");
    }
  });
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

    await chooseWorkspace(rendered.container, "ws-1");
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
    await chooseWorkspace(rendered.container, "ws-1");
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

    await chooseWorkspace(rendered.container, "ws-1");
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
    await chooseWorkspace(rendered.container, "ws-1");
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
    await chooseWorkspace(rendered.container, "ws-1");
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

async function selectValue(element: HTMLSelectElement | null, value: string): Promise<void> {
  if (!element) {
    throw new Error("select element is missing");
  }
  const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set;
  await act(async () => {
    setter?.call(element, value);
    element.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await flush();
}

describe("S02 discovery views", () => {
  it("separates install status, version and probe state, and marks the profile unconfirmed", async () => {
    const api = new FakeApi();
    api.installations = page([
      installationView(),
      installationView({
        installationId: "install-2",
        agentId: "claude",
        product: "Claude Code",
        signatureId: "claude-code",
        status: "missing-target",
        version: null,
        versionSource: "none",
        probe: {
          status: "not-attempted",
          reason: "no probe has run for this entry",
          checkedAt: null,
          durationMs: null,
          arguments: [],
        },
        issueCode: "missing-target",
        issueReason: "shim target does not exist",
      }),
      installationView({
        installationId: "install-3",
        agentId: "codebuddy",
        product: "CodeBuddy Code",
        signatureId: "codebuddy-code",
        status: "historical-residue",
        entryKind: "node-package",
        version: null,
        versionSource: "none",
      }),
    ]);
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-installations"));

    expect(byTestId(rendered.container, "installations-view")).not.toBeNull();
    expect(byTestIdPrefix(rendered.container, "installation-row-codex")).not.toBeNull();
    expect(byTestIdPrefix(rendered.container, "installation-version-codex")?.textContent).toBe(
      "0.166.0",
    );
    expect(byTestIdPrefix(rendered.container, "installation-status-claude")?.textContent).toContain(
      "入口文件缺失",
    );
    expect(
      byTestIdPrefix(rendered.container, "installation-status-codebuddy")?.textContent,
    ).toContain("历史残留");
    const text = rendered.container.textContent ?? "";
    expect(text).toContain("unconfirmed-local");
    expect(text).toContain("shim target does not exist");
    // A version is never invented when the probe did not succeed.
    expect(byTestIdPrefix(rendered.container, "installation-version-claude")?.textContent).toBe(
      "未取得",
    );
  });

  it("keeps an installation error visible and retries it", async () => {
    const api = new FakeApi();
    // The view reloads once the first scan generation arrives, so a broken
    // backend fails both of those reads before the user retries.
    api.installationFailures = 2;
    api.installations = page([installationView()]);
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-installations"));
    expect(byTestId(rendered.container, "installations-error")).not.toBeNull();
    expect(byTestId(rendered.container, "installations-empty")).toBeNull();
    await click(byTestId(rendered.container, "installations-retry"));
    expect(byTestId(rendered.container, "installations-error")).toBeNull();
    expect(byTestIdPrefix(rendered.container, "installation-row-codex")).not.toBeNull();
  });

  it("shows an unconfirmed process candidate without inventing an identity or a session", async () => {
    const api = new FakeApi();
    api.processes = page([
      processView(),
      processView({
        identity: null,
        pid: 5151,
        agentId: "claude",
        product: "Claude Code",
        installationId: null,
        entryKind: null,
        recognition: null,
        evidence: ["path-entry+filename"],
        state: "unknown",
        freshness: "stale",
        stateReason: "access-denied",
        parentIdentity: null,
        parentNote: "identity-unconfirmed",
        sourceProgram: null,
        workspaceId: null,
      }),
    ]);
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-processes"));
    await flush();

    expect(byTestId(rendered.container, "processes-view")).not.toBeNull();
    expect(
      rendered.container.querySelectorAll('[data-testid^="process-row-"]').length,
    ).toBe(2);
    expect(byTestId(rendered.container, "process-workspace")?.textContent).toBe("未关联");
    expect(byTestId(rendered.container, "process-source-program")?.textContent).toBe(
      "powershell.exe",
    );
    expect(byTestId(rendered.container, "process-creation-time")?.textContent).toBe(
      "2026-11-02T01:00:00.1234567Z",
    );
    const text = rendered.container.textContent ?? "";
    expect(text).toContain("未确认候选");
    expect(text).toContain("access-denied");
    expect(text).not.toContain("session");
    // Read-only: no control that could signal an external process.
    for (const id of ["process-kill", "process-send", "process-attach", "process-takeover"]) {
      expect(byTestId(rendered.container, id)).toBeNull();
    }
  });

  it("surfaces a whole-scan failure and recovers on retry", async () => {
    const api = new FakeApi();
    api.refreshFailure = {
      code: "io",
      message: "process enumeration failed",
      retryable: true,
      details: { stage: "enumerateProcesses" },
    };
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-processes"));
    await flush();
    expect(byTestId(rendered.container, "scan-error")).not.toBeNull();
    expect(byTestId(rendered.container, "scan-error-code")?.textContent).toBe("io");
    await click(byTestId(rendered.container, "scan-retry"));
    expect(byTestId(rendered.container, "scan-error")).toBeNull();
    expect(byTestId(rendered.container, "scan-status-badge")?.textContent).toBe("正常");
  });

  it("changes the bounded scan interval through the settings command", async () => {
    const api = new FakeApi();
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-processes"));
    await flush();
    await selectValue(
      byTestId<HTMLSelectElement>(rendered.container, "scan-interval"),
      "10000",
    );
    expect(api.settingsUpdates).toEqual([10000]);
  });

  it("keeps the S01 session entry working next to the discovery entries", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    api.sessions = page([summary("s-1", "a-1", "导航回归")]);
    api.details["s-1"] = detail("s-1", "a-1", "导航回归");
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-processes"));
    await click(byTestId(rendered.container, "view-sessions"));
    await chooseWorkspace(rendered.container, "ws-1");
    await click(byTestId(rendered.container, "session-row-s-1"));
    expect(byTestId(rendered.container, "detail-session-id")?.textContent).toBe("s-1");
    expect(byTestId(rendered.container, "detail-count-messages")?.textContent).toBe("2");
  });
});

describe("S03 history index", () => {
  const historyStatusValue = (overrides: Partial<HistoryOverallStatus> = {}): HistoryOverallStatus => ({
    schemaVersion: 1,
    scope: "local-windows",
    inFlight: false,
    sources: [
      {
        sourceId: "hsrc-1",
        agentId: "codex",
        product: "codex",
        enabled: true,
        status: historyStatus(),
      },
    ],
    note: "status note",
    ...overrides,
  });

  async function typeInto(input: HTMLInputElement, value: string): Promise<void> {
    const setter = Object.getOwnPropertyDescriptor(
      window.HTMLInputElement.prototype,
      "value",
    )?.set;
    await act(async () => {
      setter?.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 400));
    });
    await flush();
  }

  it("adds a source, searches, opens the same session and only resolves a resume target", async () => {
    const api = new FakeApi();
    api.historySources = page([historySource()]);
    api.historyStatusValue = historyStatusValue();
    api.historyCandidates = [
      {
        agentId: "codex",
        product: "codex",
        displayName: "Codex CLI",
        root: {
          hostId: "local-windows",
          displayPath: "C:\\Users\\me\\.codex\\sessions",
          canonicalPath: "C:\\Users\\me\\.codex\\sessions",
        },
        exists: true,
        readable: true,
        entrySample: 3,
        suggestion: "home-default",
        alreadyConfigured: false,
        note: "a candidate directory",
      },
    ];
    api.historySearchResult = {
      items: [searchHit()],
      nextCursor: null,
      total: 1,
      matchMode: "fts",
      note: "search note",
    };
    api.historyMessages = {
      items: [
        historyMessage("hmsg-1", 0, "历史记录里的中文搜索"),
        historyMessage("hmsg-2", 1, "回答"),
      ],
      nextCursor: null,
      total: 2,
      anchorMessageId: null,
      anchorIndex: null,
    };
    rendered = await renderApp(api);

    await click(byTestId(rendered.container, "view-history"));
    expect(byTestId(rendered.container, "history-view")).not.toBeNull();
    expect(byTestId(rendered.container, "history-source-hsrc-1")).not.toBeNull();
    // The unconfirmed profile is shown as such, not as "default".
    expect(rendered.container.textContent).toContain("配置尚未确认");

    const input = byTestId<HTMLInputElement>(rendered.container, "history-search-input");
    expect(input).not.toBeNull();
    await typeInto(input as HTMLInputElement, "历史记录");
    expect(api.historySearches.at(-1)?.text).toBe("历史记录");
    expect(byTestId(rendered.container, "history-hit-hmsg-1")).not.toBeNull();

    await click(byTestId(rendered.container, "history-hit-hmsg-1"));
    expect(api.historyMessageCalls.at(-1)?.anchorMessageId).toBe("hmsg-1");
    expect(byTestId(rendered.container, "history-detail-title")?.textContent).toContain(
      "历史会话标题",
    );
    expect(byTestId(rendered.container, "history-anchored")).not.toBeNull();
    expect(byTestId(rendered.container, "history-message-hmsg-1")?.dataset.anchored).toBe(
      "true",
    );

    await click(byTestId(rendered.container, "history-resume-check"));
    expect(byTestId(rendered.container, "history-resume-status")?.textContent).toBe("可解析");
    expect(byTestId(rendered.container, "history-resume-disabled")).toBeNull();
    expect(byTestId(rendered.container, "history-resume-check")?.textContent).toBe("继续会话");
    expect(byTestId(rendered.container, "runtime-run-header")).toBeNull();

    // Rebuild asks for confirmation before it clears the derived index.
    await click(byTestId(rendered.container, "history-rebuild-hsrc-1"));
    expect(byTestId(rendered.container, "history-rebuild-confirm-hsrc-1")).not.toBeNull();
    expect(api.historyRebuilds).toEqual([]);
    await click(byTestId(rendered.container, "history-rebuild-confirm-hsrc-1"));
    expect(api.historyRebuilds).toEqual(["hsrc-1"]);
  });

  it("resumes a resolved historical session only after an explicit click and routes to the new run", async () => {
    const api = new FakeApi();
    api.historySources = page([historySource()]);
    api.sessions = page([summary("hist-session-1", "codex", "历史会话标题")]);
    api.historyResume = resumeTarget({ executionAvailable: true });
    const resumed: RuntimeRunView = {
      schemaVersion: 1, runId: "resumed-run", sessionId: "hist-session-1", agentId: "codex", installationId: "install-codex",
      product: "codex", productName: "Codex CLI", transport: "native", title: "Resumed historical session",
      cwd: {hostId:"local-windows",displayPath:"C:\\test",canonicalPath:"C:\\test"}, executionState:"running", interactionState:"idle",
      turn:null,nativeSessionId:"native-1",process:null,launchArgv:[],messages:[],tools:[],permissions:[],lastSeq:0,journalTruncated:false,
      startedAt:"2026-11-03T00:00:00Z",endedAt:null,resumable:false,notes:[],error:null,
    };
    const start = vi.spyOn(api,"runtimeResume").mockResolvedValue(resumed);
    vi.spyOn(api,"runtimeGet").mockResolvedValue(resumed);
    const send = vi.spyOn(api,"runtimeSend");
    rendered = await renderApp(api);
    await click(byTestId(rendered.container,"view-history"));
    await click(byTestId(rendered.container,"history-session-hist-session-1"));
    expect(start).not.toHaveBeenCalled();
    await click(byTestId(rendered.container,"history-resume-check"));
    await flush();
    expect(start).toHaveBeenCalledTimes(1);
    expect(start).toHaveBeenCalledWith({sessionId:"hist-session-1",prompt:undefined});
    expect(send).not.toHaveBeenCalled();
    expect(byTestId(rendered.container,"runtime-run-header")?.textContent).toContain(resumed.title);
  });

  it("does not execute an unknown resume target even when an execution flag is present", async () => {
    const api = new FakeApi();
    api.sessions = page([summary("hist-session-1","codex","历史会话标题")]);
    api.historyResume = resumeTarget({status:"unknown",executionAvailable:true});
    const start=vi.spyOn(api,"runtimeResume");
    rendered=await renderApp(api);
    await click(byTestId(rendered.container,"view-history"));
    await click(byTestId(rendered.container,"history-session-hist-session-1"));
    await click(byTestId(rendered.container,"history-resume-check"));
    expect(byTestId(rendered.container, "history-resume-status")?.textContent).toBe("不确定");
    expect(start).not.toHaveBeenCalled();
  });

  it("shows a real search error and recovers on retry", async () => {
    const api = new FakeApi();
    api.historySources = page([historySource()]);
    api.historyStatusValue = historyStatusValue();
    api.historySearchFailure = 1;
    api.historySearchResult = {
      items: [searchHit()],
      nextCursor: null,
      total: 1,
      matchMode: "like",
      note: "search note",
    };
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-history"));
    await typeInto(
      byTestId<HTMLInputElement>(rendered.container, "history-search-input") as HTMLInputElement,
      "中文",
    );
    expect(byTestId(rendered.container, "history-search-error-code")?.textContent).toBe(
      "database",
    );
    expect(byTestId(rendered.container, "history-search-empty")).toBeNull();
    // A failure must not be summarised as "0 results" next to the error panel.
    expect(byTestId(rendered.container, "history-search-total")).toBeNull();
    await click(byTestId(rendered.container, "history-search-retry"));
    expect(byTestId(rendered.container, "history-search-error")).toBeNull();
    expect(byTestId(rendered.container, "history-hit-hmsg-1")).not.toBeNull();
    expect(api.historySearches.at(-1)?.text).toBe("中文");
  });

  it("distinguishes an empty source list from a failed one", async () => {
    const api = new FakeApi();
    api.historySourcesFailure = 1;
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-history"));
    expect(byTestId(rendered.container, "history-sources-error-code")?.textContent).toBe(
      "database",
    );
    expect(byTestId(rendered.container, "history-sources-empty")).toBeNull();
    await click(byTestId(rendered.container, "history-sources-retry"));
    expect(byTestId(rendered.container, "history-sources-empty")).not.toBeNull();
    expect(byTestId(rendered.container, "history-browse-empty")).not.toBeNull();
    expect(byTestId(rendered.container, "history-detail-empty")).not.toBeNull();
  });

  it("shows a blocked resume target without offering execution", async () => {
    const api = new FakeApi();
    api.historySources = page([historySource()]);
    api.historyStatusValue = historyStatusValue();
    api.historyResume = resumeTarget({
      status: "blocked",
      reason: "an active run can still write this session",
      conflicts: [{ kind: "active-run", detail: "run run-1 is running" }],
    });
    api.sessions = page([summary("hist-session-1", "codex", "历史会话标题")]);
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-history"));
    await click(byTestId(rendered.container, "history-session-hist-session-1"));
    await click(byTestId(rendered.container, "history-resume-check"));
    expect(byTestId(rendered.container, "history-resume-status")?.textContent).toBe("被阻止");
    expect(rendered.container.textContent).toContain("该会话仍有活动运行");
    expect(byTestId(rendered.container, "history-resume-disabled")).toBeNull();
    expect(byTestId(rendered.container, "runtime-run-header")).toBeNull();
  });

  it("pages messages with a cursor without duplicating or reordering rows", async () => {
    const api = new FakeApi();
    api.historySources = page([historySource()]);
    api.historyStatusValue = historyStatusValue();
    api.sessions = page([summary("hist-session-1", "codex", "历史会话标题")]);
    const firstPage: MessagePage = {
      items: [historyMessage("hmsg-1", 0, "第一条"), historyMessage("hmsg-2", 1, "第二条")],
      nextCursor: "cursor-1",
      total: 3,
      anchorMessageId: null,
      anchorIndex: null,
    };
    const secondPage: MessagePage = {
      items: [historyMessage("hmsg-3", 2, "第三条")],
      nextCursor: null,
      total: 3,
      anchorMessageId: null,
      anchorIndex: null,
    };
    api.listSessionMessages = (query) => {
      api.historyMessageCalls.push(query);
      return Promise.resolve(query.cursor ? secondPage : firstPage);
    };
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-history"));
    await click(byTestId(rendered.container, "history-session-hist-session-1"));

    const rowIds = () =>
      Array.from(
        rendered!.container.querySelectorAll<HTMLElement>(
          "[data-testid^='history-message-hmsg-']",
        ),
      ).map((row) => row.dataset.testid);
    expect(api.historyMessageCalls[0]?.limit).toBe(50);
    expect(api.historyMessageCalls[0]?.cursor).toBeUndefined();
    expect(rowIds()).toEqual([
      "history-message-hmsg-1",
      "history-message-hmsg-2",
    ]);

    await click(byTestId(rendered.container, "history-messages-load-more"));
    expect(api.historyMessageCalls.at(-1)?.cursor).toBe("cursor-1");
    expect(rowIds()).toEqual([
      "history-message-hmsg-1",
      "history-message-hmsg-2",
      "history-message-hmsg-3",
    ]);
    expect(byTestId(rendered.container, "history-messages-load-more")).toBeNull();
  });

  it("shows stale, error and unsupported states distinctly", async () => {
    const api = new FakeApi();
    api.historySources = page([
      historySource({ sourceId: "hsrc-stale", enabled: false }),
      historySource({
        sourceId: "hsrc-error",
        status: historyStatus({
          status: "error",
          error: {
            code: "access_denied",
            message: "the root cannot be listed",
            retryable: false,
            details: { stage: "historyScan" },
          },
        }),
      }),
    ]);
    api.historyStatusValue = historyStatusValue({
      sources: [
        {
          sourceId: "hsrc-stale",
          agentId: "codex",
          product: "codex",
          enabled: false,
          status: historyStatus(),
        },
        {
          sourceId: "hsrc-error",
          agentId: "codex",
          product: "codex",
          enabled: true,
          status: historyStatus({
            status: "error",
            error: {
              code: "access_denied",
              message: "the root cannot be listed",
              retryable: false,
              details: { stage: "historyScan" },
            },
          }),
        },
      ],
    });
    api.sessions = page([summary("hist-session-1", "codex", "历史会话标题")]);
    api.historyResume = resumeTarget({
      status: "unsupported",
      reason: "the session has no native id",
      conflicts: [{ kind: "missing-native-id", detail: "nativeId is null" }],
    });
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-history"));

    const stale = byTestId(rendered.container, "history-source-hsrc-stale");
    expect(stale?.textContent).toContain("已过期");
    expect(stale?.textContent).toContain("已停用");
    const failed = byTestId(rendered.container, "history-source-hsrc-error");
    expect(failed?.textContent).toContain("失败");
    expect(failed?.textContent).toContain("access_denied");

    await click(byTestId(rendered.container, "history-session-hist-session-1"));
    await click(byTestId(rendered.container, "history-resume-check"));
    expect(byTestId(rendered.container, "history-resume-status")?.textContent).toBe("不支持");
    expect(byTestId(rendered.container, "history-resume-disabled")).toBeNull();
    expect(byTestId(rendered.container, "runtime-run-header")).toBeNull();
  });

  it("sends agent, source and workspace filters with the search query", async () => {
    async function choose(element: HTMLSelectElement | null, value: string): Promise<void> {
      if (!element) {
        throw new Error("select is missing");
      }
      const setter = Object.getOwnPropertyDescriptor(
        window.HTMLSelectElement.prototype,
        "value",
      )?.set;
      await act(async () => {
        setter?.call(element, value);
        element.dispatchEvent(new Event("change", { bubbles: true }));
      });
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 400));
      });
      await flush();
    }

    const api = new FakeApi();
    api.historySources = page([
      historySource(),
      historySource({
        sourceId: "hsrc-2",
        agentId: "claude",
        product: "claude-code",
        root: {
          hostId: "local-windows",
          displayPath: "C:\Users\me\.claude\projects",
          canonicalPath: "C:\Users\me\.claude\projects",
        },
      }),
    ]);
    api.historyStatusValue = historyStatusValue();
    api.workspaces = page([workspace("ws-1", "C:\Work\Alpha")]);
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-history"));

    await typeInto(
      byTestId<HTMLInputElement>(rendered.container, "history-search-input") as HTMLInputElement,
      "中文",
    );
    expect(api.historySearches.at(-1)?.text).toBe("中文");

    await choose(byTestId<HTMLSelectElement>(rendered.container, "history-filter-agent"), "claude");
    expect(api.historySearches.at(-1)?.agentId).toBe("claude");
    await choose(byTestId<HTMLSelectElement>(rendered.container, "history-filter-source"), "hsrc-2");
    expect(api.historySearches.at(-1)?.sourceId).toBe("hsrc-2");
    await choose(byTestId<HTMLSelectElement>(rendered.container, "history-filter-workspace"), "ws-1");
    expect(api.historySearches.at(-1)?.workspaceId).toBe("ws-1");
    expect(api.historySearches.at(-1)?.text).toBe("中文");
    // Filters live in the query key, so every change is a fresh first page.
    expect(api.historySearches.at(-1)?.cursor).toBeUndefined();
  });
});

describe("S03-R1 bounded session detail", () => {
  it("reads a session through metadata plus one bounded message page", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    api.sessions = page([summary("s-big", "a-1", "大历史会话")]);
    api.details["s-big"] = detail("s-big", "a-1", "大历史会话");
    const all = api.details["s-big"].messages;
    const calls: MessagePageQuery[] = [];
    api.listSessionMessages = (query) => {
      calls.push(query);
      return Promise.resolve(
        query.cursor
          ? {
              items: all.slice(1),
              nextCursor: null,
              total: all.length,
              anchorMessageId: null,
              anchorIndex: null,
            }
          : {
              items: all.slice(0, 1),
              nextCursor: "page-2",
              total: all.length,
              anchorMessageId: null,
              anchorIndex: null,
            },
      );
    };
    rendered = await renderApp(api);
    await chooseWorkspace(rendered.container, "ws-1");
    await click(byTestId(rendered.container, "session-row-s-big"));

    // The detail no longer calls getSession (unbounded); it asks for metadata
    // and a bounded page instead.
    expect(calls.at(-1)?.sessionId).toBe("s-big");
    expect(calls.at(-1)?.limit).toBe(100);
    expect(byTestId(rendered.container, "detail-count-messages")?.textContent).toBe(
      String(all.length),
    );
    expect(
      byTestId(rendered.container, "detail-messages-loaded")?.textContent,
    ).toContain(`1/${all.length}`);

    await click(byTestId(rendered.container, "detail-load-more"));
    expect(
      byTestId(rendered.container, "detail-messages-loaded")?.textContent,
    ).toContain(`${all.length}/${all.length}`);
    expect(byTestId(rendered.container, "detail-load-more")).toBeNull();
  });

  it("drops a late detail page from a previously selected session", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    api.sessions = page([
      summary("s-a", "a-1", "会话 A"),
      summary("s-b", "a-1", "会话 B"),
    ]);
    api.details["s-a"] = detail("s-a", "a-1", "会话 A");
    api.details["s-b"] = detail("s-b", "a-1", "会话 B");
    let releaseA: ((page: MessagePage) => void) | null = null;
    api.listSessionMessages = (query) => {
      if (query.sessionId === "s-a") {
        return new Promise<MessagePage>((resolve) => {
          releaseA = resolve;
        });
      }
      return Promise.resolve({
        items: api.details["s-b"].messages,
        nextCursor: null,
        total: 2,
        anchorMessageId: null,
        anchorIndex: null,
      });
    };
    rendered = await renderApp(api);
    await chooseWorkspace(rendered.container, "ws-1");
    await click(byTestId(rendered.container, "session-row-s-a"));
    await click(byTestId(rendered.container, "session-row-s-b"));
    expect(byTestId(rendered.container, "detail-session-id")?.textContent).toBe("s-b");

    await act(async () => {
      releaseA?.({
        items: api.details["s-a"].messages,
        nextCursor: null,
        total: 2,
        anchorMessageId: null,
        anchorIndex: null,
      });
    });
    // The stale answer belongs to s-a and must not replace s-b's detail.
    expect(byTestId(rendered.container, "detail-session-id")?.textContent).toBe("s-b");
    expect(byTestId(rendered.container, "message-row-m-1")).not.toBeNull();
  });
});

describe("S04 managed terminal entry", () => {
  it("renders the terminal workspace and its empty state from the navigation", async () => {
    const api = new FakeApi();
    rendered = await renderApp(api);
    const tab = byTestId(rendered.container, "view-terminal");
    expect(tab).not.toBeNull();
    await click(tab);
    expect(byTestId(rendered.container, "terminal-workspace")).not.toBeNull();
    expect(byTestId(rendered.container, "terminal-empty")).not.toBeNull();
    expect(byTestId(rendered.container, "terminal-new")).not.toBeNull();
    // No terminal is created just by opening the page.
    expect(byTestId(rendered.container, "terminal-tab-term-1")).toBeNull();
  });

  it("keeps the stop controls disabled without a terminal", async () => {
    const api = new FakeApi();
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "view-terminal"));
    for (const id of [
      "terminal-stop-interrupt",
      "terminal-stop-gentle",
      "terminal-stop-force",
      "terminal-close",
    ]) {
      expect((byTestId(rendered.container, id) as HTMLButtonElement).disabled).toBe(true);
    }
  });

  it("keeps the terminal workspace mounted while another page is shown", async () => {
    const api = new FakeApi();
    rendered = await renderApp(api);
    const host = byTestId(rendered.container, "terminal-host");
    expect(host).not.toBeNull();
    expect(host?.className).toContain("hidden");

    await click(byTestId(rendered.container, "view-terminal"));
    expect(byTestId(rendered.container, "terminal-host")?.className).not.toContain("hidden");
    expect(byTestId(rendered.container, "terminal-workspace")).not.toBeNull();

    // Switching away must hide, not unmount: an unmounted workspace would drop
    // every tab while the PTYs keep running.
    await click(byTestId(rendered.container, "view-sessions"));
    const afterSwitch = byTestId(rendered.container, "terminal-host");
    expect(afterSwitch).not.toBeNull();
    expect(afterSwitch?.className).toContain("hidden");
    expect(byTestId(rendered.container, "terminal-workspace")).not.toBeNull();
    expect(byTestId(rendered.container, "terminal-empty")).not.toBeNull();
  });

  it("quits directly when no terminal is owned, and asks otherwise", async () => {
    const api = new FakeApi();
    rendered = await renderApp(api);
    await click(byTestId(rendered.container, "app-quit"));
    expect(api.quitCalls).toBe(1);
    expect(byTestId(rendered.container, "quit-dialog")).toBeNull();

    api.quitCheck = {
      activeCount: 2,
      needsConfirmation: true,
      note: "2 terminals are still running",
    };
    await click(byTestId(rendered.container, "app-quit"));
    expect(byTestId(rendered.container, "quit-dialog")).not.toBeNull();
    expect(api.quitCalls).toBe(1);
    await click(byTestId(rendered.container, "quit-cancel"));
    expect(byTestId(rendered.container, "quit-dialog")).toBeNull();
    expect(api.quitCalls).toBe(1);

    await click(byTestId(rendered.container, "app-quit"));
    await click(byTestId(rendered.container, "quit-confirm"));
    expect(api.quitCalls).toBe(2);
  });
});


describe("centered CLI home", () => {
  it("opens on the icon composer and keeps the project tree in the shared sidebar", async () => {
    const api = new FakeApi();
    api.workspaces = page([workspace("ws-1", "C:\\Work\\Alpha")]);
    const start = vi.spyOn(api, "runtimeStart");
    rendered = await renderApp(api, false);
    const home = byTestId(rendered.container, "home-view")!;
    expect(home.parentElement?.className).not.toContain("hidden");
    await click(byTestId(home, "cli-select-claude-code"));
    expect(start).not.toHaveBeenCalled();
    await click(byTestId(rendered.container, "view-sessions"));
    await chooseWorkspace(rendered.container, "ws-1");
    const project = byTestId(rendered.container, "workspace-row-ws-1")!;
    expect(project.closest("nav")).not.toBeNull();
    expect(byTestId(rendered.container, "session-browser")!.contains(project)).toBe(false);
    await click(byTestId(rendered.container, "view-home"));
    expect(byTestId(home, "cli-select-claude-code")?.getAttribute("aria-pressed")).toBe("true");
  });

  it("dispatches one explicit start through the mounted runtime under StrictMode and preserves a failed draft", async () => {
    const api = new FakeApi();
    vi.spyOn(api, "runtimeCapabilities").mockResolvedValue({schemaVersion: 1, note: "fixture", items: [{
      product: "claude-code", productName: "Claude Code", agentId: "claude", installationId: "claude-install",
      version: "1", executable: null, transport: "native", structuredChat: "unverified", permissionFlow: "unverified",
      toolEvents: "unverified", cancel: "unverified", resume: "unverified", evidence: "fixture", entry: null, reason: null, notes: [],
    }]});
    let reject!: (reason: unknown) => void;
    const start = vi.spyOn(api, "runtimeStart").mockImplementation(() => new Promise<RuntimeRunView>((_, fail) => { reject = fail; }));
    rendered = await renderApp(api, false, true);
    const home = byTestId(rendered.container, "home-view")!;
    const fill = async (id: string, value: string, textarea = false) => {
      const element = byTestId(home, id)!;
      await act(async () => {
        Object.getOwnPropertyDescriptor(textarea ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype, "value")!.set!.call(element, value);
        element.dispatchEvent(new Event("input", { bubbles: true }));
      });
    };
    await fill("runtime-cwd-input", "D:\\项目");
    await fill("runtime-prompt-input", "检查 UI 🧭", true);
    await act(async () => {
      byTestId<HTMLButtonElement>(home, "runtime-start")!.click();
      byTestId<HTMLButtonElement>(home, "runtime-start")!.click();
    });
    expect(start).toHaveBeenCalledExactlyOnceWith({ installationId: "claude-install", cwd: "D:\\项目", prompt: "检查 UI 🧭" });
    expect(byTestId(rendered.container, "runtime-host")?.className).not.toContain("hidden");
    await click(byTestId(rendered.container, "view-home"));
    expect(byTestId<HTMLButtonElement>(home, "runtime-start")!.disabled).toBe(true);
    await act(async () => reject({ code: "unsupported", message: "fixture failure", retryable: false, details: {} }));
    await flush();
    expect(byTestId<HTMLTextAreaElement>(home, "runtime-prompt-input")!.value).toBe("检查 UI 🧭");
    expect(byTestId<HTMLButtonElement>(home, "runtime-start")!.disabled).toBe(false);
    await click(byTestId(rendered.container, "view-runtime"));
    expect(rendered.container.textContent).toContain("fixture failure");
  });
});
