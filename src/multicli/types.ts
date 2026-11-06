/**
 * Wire types for the frozen MultiCLI domain contract
 * (`docs/contracts/domain.schema.json`). Field names are camelCase on the
 * IPC boundary; the UI never invents fields the backend does not return.
 */

export type SessionOrigin = "managed" | "external-observed" | "history-only";
export type RunTransport = "native" | "acp" | "pty";
export type RunOwnership = "managed" | "external-observed";
export type ExecutionStatus =
  | "starting"
  | "running"
  | "stopping"
  | "exited"
  | "failed"
  | "unknown";
export type InteractionStatus =
  | "working"
  | "awaiting-permission"
  | "awaiting-input"
  | "idle"
  | "unknown";
export type EvidenceLevel = "observed" | "inferred";
export type CapabilityName =
  | "installedDetection"
  | "processDetection"
  | "historyRead"
  | "historySearch"
  | "resume"
  | "structuredChat"
  | "permissionFlow"
  | "pty"
  | "quotaRead";
export type CapabilityStatus = "supported" | "unsupported" | "unverified";
export type CapabilityEvidenceLevel = "document" | "fixture" | "live";

export type ErrorCode =
  | "unsupported"
  | "unverified"
  | "invalid_argument"
  | "not_found"
  | "access_denied"
  | "conflict"
  | "timeout"
  | "cancelled"
  | "io"
  | "database"
  | "protocol";

export interface AppError {
  code: ErrorCode;
  message: string;
  retryable: boolean;
  details: Record<string, unknown>;
}

export interface HostPath {
  hostId: string;
  displayPath: string;
  canonicalPath: string | null;
}

/** `hostId + pid + OS creationTime`; PID alone never identifies a process. */
export interface ProcessIdentity {
  hostId: string;
  pid: number;
  creationTime: string;
}

export interface AgentDefinition {
  id: string;
  vendor: string;
  displayName: string;
}

export interface Workspace {
  id: string;
  path: HostPath;
  repoGroupId: string | null;
  worktreeId: string | null;
}

export interface Session {
  id: string;
  agentId: string;
  hostId: string;
  sourceNamespace: string;
  nativeId: string | null;
  origin: SessionOrigin;
  title: string;
}

export interface SessionWorkspace {
  sessionId: string;
  workspaceId: string;
  role: "primary" | "related";
  evidence: string;
}

export interface Run {
  id: string;
  sessionId: string;
  installationId: string | null;
  transport: RunTransport;
  ownership: RunOwnership;
  startedAt: string | null;
  endedAt: string | null;
  executionStatus: ExecutionStatus;
  interactionStatus: InteractionStatus;
  evidenceLevel: EvidenceLevel;
  evidence: string | null;
}

export interface Message {
  id: string;
  sessionId: string;
  runId: string | null;
  nativeEventId: string | null;
  sourceKey: string;
  seq: number;
  kind: string;
  content: Record<string, unknown>;
  sourceRef: string | null;
  observedAt: string;
}

export interface Capability {
  agentId: string;
  installationId: string | null;
  name: CapabilityName;
  status: CapabilityStatus;
  versionRange: string | null;
  evidenceLevel: CapabilityEvidenceLevel;
  source: string;
  verifiedAt: string | null;
  reason: string | null;
}

export interface WorkspaceListItem {
  workspace: Workspace;
  sessionCount: number;
}

export interface AgentListItem {
  agent: AgentDefinition;
  installationCount: number;
  sessionCount: number;
}

export interface SessionSummary {
  session: Session;
  primaryWorkspaceId: string | null;
  workspaceIds: string[];
  messageCount: number;
  runCount: number;
}

export interface SessionDetail {
  session: Session;
  workspaces: SessionWorkspace[];
  runs: Run[];
  messages: Message[];
  messageCount: number;
  runCount: number;
}

export interface Page<T> {
  items: T[];
  nextCursor: string | null;
  total: number;
}

export interface PageQuery {
  limit: number;
  cursor?: string | null;
}

export interface SessionQuery extends PageQuery {
  workspaceId?: string | null;
  agentId?: string | null;
  origin?: SessionOrigin | null;
}

export interface CapabilityQuery extends PageQuery {
  agentId: string;
}

export interface DiagnosticsExport {
  path: string;
  createdAt: string;
}

// ---------------------------------------------------------------------------
// S02 discovery views (`docs/contracts/discovery-v1.md`). Kept separate from
// the frozen domain DTOs: an external CLI never claims a chat session.
// ---------------------------------------------------------------------------

export type InstallStatus =
  | "installed"
  | "unverified"
  | "missing-target"
  | "historical-residue";

export type ProbeStatus = "ok" | "failed" | "timeout" | "skipped" | "not-attempted";

export type DiscoveryEntryKind =
  | "native-exe"
  | "node-package"
  | "cmd-wrapper"
  | "powershell-wrapper";

export type ProcessState = "alive" | "exited" | "unknown";
export type Freshness = "fresh" | "stale";
export type RecognitionStrength = "confirmed" | "likely";
export type ScanStatus = "idle" | "scanning" | "ok" | "partial" | "error";

export interface ProbeView {
  status: ProbeStatus;
  reason: string | null;
  checkedAt: string | null;
  durationMs: number | null;
  arguments: string[];
}

export interface InstallationDiscoveryView {
  installationId: string | null;
  agentId: string;
  product: string;
  signatureId: string;
  status: InstallStatus;
  entryKind: DiscoveryEntryKind;
  executable: HostPath | null;
  runtime: HostPath | null;
  entryScript: HostPath | null;
  version: string | null;
  versionSource: "probe" | "none";
  probe: ProbeView;
  configProfileId: string;
  configProfileStatus: "confirmed" | "unconfirmed";
  issueCode: string | null;
  issueReason: string | null;
  discoveredFrom: string;
  discoveredAt: string;
  lastSeenScan: number;
}

export interface ProcessDiscoveryView {
  identity: ProcessIdentity | null;
  pid: number | null;
  agentId: string | null;
  product: string | null;
  installationId: string | null;
  entryKind: DiscoveryEntryKind | null;
  recognition: RecognitionStrength | null;
  evidence: string[];
  state: ProcessState;
  freshness: Freshness;
  stateReason: string | null;
  executable: HostPath | null;
  redactedCommand: string | null;
  parentIdentity: ProcessIdentity | null;
  parentNote: string | null;
  sourceProgram: string | null;
  workspaceId: string | null;
  workspaceEvidence: string | null;
  /** Always null in S02; an observation view never claims a chat session. */
  sessionId: string | null;
  firstSeenAt: string;
  lastSeenAt: string;
  lastSeenScan: number;
}

export interface ScanView {
  generation: number;
  status: ScanStatus;
  inFlight: boolean;
  lastAttemptAt: string | null;
  lastSuccessAt: string | null;
  error: AppError | null;
  installError: AppError | null;
  intervalMs: number;
  scope: string;
  sampledProcesses: number;
  unreadableProcesses: number;
  trackedProcesses: number;
  note: string;
}

export interface ProcessDiscoveryPage {
  items: ProcessDiscoveryView[];
  nextCursor: string | null;
  total: number;
  scan: ScanView;
}

export interface InstallationQuery extends PageQuery {
  agentId?: string | null;
}

export interface ProcessQuery extends PageQuery {
  agentId?: string | null;
  workspaceId?: string | null;
  state?: ProcessState | null;
}

export interface DiscoverySettings {
  intervalMs: number;
  minIntervalMs: number;
  maxIntervalMs: number;
  note: string;
}

export const MAX_PAGE_LIMIT = 200;

// ---------------------------------------------------------------------------
// S03 history views (`docs/contracts/history-v1.md`). Separate from the frozen
// S01 DTOs and from the S02 discovery views.
// ---------------------------------------------------------------------------

export type HistoryIndexStatus =
  | "idle"
  | "indexing"
  | "ok"
  | "partial"
  | "error"
  | "cancelled";

export interface HistoryStatusView {
  generation: number;
  status: HistoryIndexStatus;
  inFlight: boolean;
  lastAttemptAt: string | null;
  lastSuccessAt: string | null;
  error: AppError | null;
  filesTotal: number;
  filesDone: number;
  filesSkipped: number;
  filesFailed: number;
  messagesIndexed: number;
  bytesRead: number;
  complete: boolean;
  progressPermille: number;
  note: string;
}

export interface HistorySourceView {
  sourceId: string;
  hostId: string;
  agentId: string;
  product: string;
  installationId: string | null;
  configProfileId: string;
  configProfileStatus: "confirmed" | "unconfirmed";
  root: HostPath;
  enabled: boolean;
  parserVersion: number;
  createdAt: string;
  updatedAt: string;
  status: HistoryStatusView;
}

export interface HistorySourceStatus {
  sourceId: string;
  agentId: string;
  product: string;
  enabled: boolean;
  status: HistoryStatusView;
}

export interface HistoryOverallStatus {
  schemaVersion: number;
  scope: string;
  inFlight: boolean;
  sources: HistorySourceStatus[];
  note: string;
}

export interface HistoryCandidateView {
  agentId: string;
  product: string;
  displayName: string;
  root: HostPath;
  exists: boolean;
  readable: boolean;
  entrySample: number;
  suggestion: string;
  alreadyConfigured: boolean;
  note: string;
}

export interface HistorySourceUpdateInput {
  sourceId?: string | null;
  product: string;
  installationId?: string | null;
  configProfileId: string;
  root: string;
  enabled: boolean;
}

export interface SessionWorkspaceDetail {
  workspaceId: string;
  role: "primary" | "related";
  evidence: string;
  path: HostPath | null;
}

export interface MessageKindCount {
  kind: string;
  count: number;
}

export interface HistoryProvenanceView {
  sourceIds: string[];
  fileCount: number;
  parserVersions: number[];
  readOnly: boolean;
}

export interface SessionMetadataView {
  session: Session;
  workspaces: SessionWorkspaceDetail[];
  runs: Run[];
  runTotal: number;
  messageCount: number;
  messageKinds: MessageKindCount[];
  firstMessageAt: string | null;
  lastMessageAt: string | null;
  history: HistoryProvenanceView | null;
  note: string;
}

export interface MessagePage {
  items: Message[];
  nextCursor: string | null;
  total: number;
  anchorMessageId: string | null;
  anchorIndex: number | null;
}

export interface MessagePageQuery {
  sessionId: string;
  limit: number;
  cursor?: string | null;
  anchorMessageId?: string | null;
}

export interface SearchHitView {
  sessionId: string;
  messageId: string;
  sessionTitle: string;
  agentId: string;
  kind: string;
  seq: number;
  observedAt: string;
  snippet: string;
  snippetTruncated: boolean;
  matchMode: "fts" | "like";
  sourceIds: string[];
  sessionMessageCount: number;
}

export interface SearchPage {
  items: SearchHitView[];
  nextCursor: string | null;
  total: number;
  matchMode: "fts" | "like";
  note: string;
}

export interface HistorySearchQuery {
  text: string;
  workspaceId?: string | null;
  agentId?: string | null;
  sourceId?: string | null;
  limit: number;
  cursor?: string | null;
}

export interface ResumeConflictView {
  kind: string;
  detail: string;
}

export interface ResumeTargetView {
  sessionId: string;
  agentId: string;
  product: string | null;
  nativeId: string | null;
  sourceNamespace: string;
  configProfileId: string | null;
  status: "resolved" | "blocked" | "unknown" | "unsupported";
  reason: string;
  /** Additive: old installed binaries may omit this field. */
  reasonCode?: string;
  installationId: string | null;
  executable: HostPath | null;
  workspaces: HostPath[];
  conflicts: ResumeConflictView[];
  evidence: string[];
  executionAvailable: boolean;
  note: string;
}

// ---------------------------------------------------------------------------
// S04 managed terminal views (`docs/contracts/terminal-v1.md`). Additive to the
// frozen S01 domain: a terminal Run is still the frozen `Run` record, and these
// are the extra terminal-specific fields the UI renders.
// ---------------------------------------------------------------------------

export type ShellFlavor = "cmd" | "windows-powershell" | "pwsh";
export type TerminalTargetKind = "shell" | "installation" | "custom-cli";
export type TerminalLaunchChain = "exe" | "cmd" | "powershell";
export type TerminalRunState = "starting" | "running" | "stopping" | "exited" | "failed";
export type TerminalStopMode = "interrupt" | "gentle" | "force";
export type TerminalJobMode = "kill-on-close" | "unassigned";

export interface TerminalTarget {
  kind: TerminalTargetKind;
  shell?: ShellFlavor | null;
  installationId?: string | null;
  customCliId?: string | null;
}

export interface TerminalEnvVar {
  name: string;
  value: string;
}

export interface TerminalOpenSpec {
  target: TerminalTarget;
  args: string[];
  cwd: string;
  env: TerminalEnvVar[];
  cols: number;
  rows: number;
  title?: string | null;
}

export interface TerminalJobGuardView {
  mode: TerminalJobMode;
  assigned: boolean;
  reason: string | null;
}

export interface TerminalBuffersView {
  bufferCapacityBytes: number;
  bufferedBytes: number;
  droppedBytes: number;
  logPath: string;
  logCapacityBytes: number;
  logBytes: number;
  logRotations: number;
  logDroppedBytes: number;
}

export interface TerminalExitView {
  code: number | null;
  success: boolean;
  endedAt: string;
  reported: boolean;
  signal: string | null;
}

export interface TerminalStopReport {
  requested: TerminalStopMode;
  interruptSent: boolean;
  escalated: boolean;
  waitedMs: number;
  alreadyTerminal: boolean;
  refusedReason: string | null;
}

export interface TerminalView {
  schemaVersion: number;
  terminalId: string;
  runId: string;
  sessionId: string;
  agentId: string;
  installationId: string | null;
  title: string;
  targetKind: TerminalTargetKind;
  product: string | null;
  executable: HostPath;
  launchChain: TerminalLaunchChain;
  launchArgv: string[];
  args: string[];
  cwd: HostPath;
  envNames: string[];
  state: TerminalRunState;
  interactionStatus: string;
  identity: ProcessIdentity | null;
  job: TerminalJobGuardView;
  buffers: TerminalBuffersView;
  exit: TerminalExitView | null;
  stop: TerminalStopReport | null;
  startedAt: string;
  endedAt: string | null;
  evidence: string;
  notes: string[];
}

export interface TerminalLimits {
  bufferBytes: number;
  minBufferBytes: number;
  maxBufferBytes: number;
  logBytes: number;
  minLogBytes: number;
  maxLogBytes: number;
  maxTerminals: number;
  note: string;
}

export interface TerminalListView {
  schemaVersion: number;
  items: TerminalView[];
  limits: TerminalLimits;
  activeCount: number;
  note: string;
}

export interface TerminalReadQuery {
  terminalId: string;
  cursor?: number | null;
  maxBytes?: number | null;
}

export interface TerminalReadView {
  terminalId: string;
  runId: string;
  streamId: string;
  fromSeq: number;
  toSeq: number;
  text: string;
  bytes: number;
  droppedBefore: number;
  droppedBytes: number;
  historyTruncated: boolean;
  bufferedBytes: number;
  state: TerminalRunState;
  exit: TerminalExitView | null;
}

export interface TerminalWriteView {
  terminalId: string;
  acceptedBytes: number;
  state: TerminalRunState;
}

export interface TerminalResizeView {
  terminalId: string;
  cols: number;
  rows: number;
}

export interface TerminalStopView {
  terminal: TerminalView;
  stop: TerminalStopReport;
}

export interface TerminalCloseView {
  terminalId: string;
  removed: boolean;
  stop: TerminalStopReport;
  note: string;
}

export interface TerminalShutdownAllView {
  stopped: TerminalCloseView[];
  remaining: number;
  note: string;
}

export interface TerminalQuitCheckView {
  activeCount: number;
  needsConfirmation: boolean;
  note: string;
}

export interface TerminalTargetView {
  id: string;
  kind: TerminalTargetKind;
  label: string;
  agentId: string | null;
  product: string | null;
  installationId: string | null;
  customCliId: string | null;
  executable: HostPath | null;
  launchChain: TerminalLaunchChain | null;
  status: "ready" | "missing" | "unsupported" | "unverified";
  reason: string | null;
  version: string | null;
}

export interface TerminalTargetsView {
  schemaVersion: number;
  items: TerminalTargetView[];
  limits: TerminalLimits;
  cwdDefault: string;
  note: string;
}

export interface TerminalNotify {
  kind: "output" | "state";
  terminalId: string;
  runId: string;
  streamId: string;
  seq: number;
  state: TerminalRunState;
  bufferedBytes: number;
  droppedBytes: number;
}

// ---------------------------------------------------------------------------
// S05 structured runtime (`docs/contracts/runtime-v1.md`)
// ---------------------------------------------------------------------------

export type RuntimeTransport = "native" | "acp";
export type RuntimeCapabilityStatus = "supported" | "unsupported" | "unverified";
export type RuntimeEvidence = "live" | "document" | "fixture";

export interface RuntimeCapabilityView {
  product: string;
  productName: string;
  agentId: string;
  installationId: string | null;
  version: string | null;
  executable: HostPath | null;
  transport: RuntimeTransport;
  structuredChat: RuntimeCapabilityStatus;
  permissionFlow: RuntimeCapabilityStatus;
  toolEvents: RuntimeCapabilityStatus;
  cancel: RuntimeCapabilityStatus;
  resume: RuntimeCapabilityStatus;
  evidence: RuntimeEvidence;
  entry: string | null;
  reason: string | null;
  notes: string[];
}

export interface RuntimeCapabilitiesView {
  schemaVersion: number;
  items: RuntimeCapabilityView[];
  note: string;
}

export interface RuntimePermissionOption {
  optionId: string;
  label: string;
  kind: string;
}

export interface RuntimePermissionView {
  permissionId: string;
  nativeRequestId: string | null;
  toolCallId: string | null;
  title: string;
  detail: string | null;
  kind: string;
  options: RuntimePermissionOption[];
  state: "pending" | "answered" | "expired" | "cancelled";
  selectedOptionId: string | null;
  autoDenied: boolean;
  reason: string | null;
  turnId: string | null;
  createdAt: string;
  resolvedAt: string | null;
}

export interface RuntimeMessageView {
  messageId: string;
  role: "assistant" | "user" | "system";
  text: string;
  complete: boolean;
  seq: number;
}

export interface RuntimeToolView {
  toolCallId: string;
  permissionId: string | null;
  authorization: string;
  execution: string;
  outcome: string;
  name: string;
  kind: string;
  status: string;
  input: unknown;
  output: string | null;
  isError: boolean;
  startedAt: string;
  endedAt: string | null;
}

export interface RuntimeTurnView {
  turnId: string;
  state: string;
  startedAt: string;
  endedAt: string | null;
  model: string | null;
  effort: string | null;
  reason: string | null;
}

export interface RuntimeRunView {
  schemaVersion: number;
  runId: string;
  sessionId: string;
  agentId: string;
  installationId: string | null;
  product: string;
  productName: string;
  transport: RuntimeTransport;
  title: string;
  cwd: HostPath;
  executionState: ExecutionStatus;
  interactionState: InteractionStatus;
  turn: RuntimeTurnView | null;
  nativeSessionId: string | null;
  process: ProcessIdentity | null;
  launchArgv: string[];
  messages: RuntimeMessageView[];
  tools: RuntimeToolView[];
  permissions: RuntimePermissionView[];
  lastSeq: number;
  journalTruncated: boolean;
  startedAt: string;
  endedAt: string | null;
  resumable: boolean;
  notes: string[];
  error: AppError | null;
}

/** Frozen event envelope (`docs/contracts/events.schema.json`). */
export interface EventEnvelope {
  schemaVersion: number;
  sessionId: string | null;
  runId: string | null;
  streamId: string;
  seq: number;
  timestamp: string;
  kind: string;
  source: string;
  nativeEventId: string | null;
  sourceKey: string;
  payload: Record<string, unknown>;
}

export interface RuntimeEventPage {
  schemaVersion: number;
  runId: string;
  sessionId: string;
  items: EventEnvelope[];
  nextCursor: number;
  droppedBefore: number;
  lastSeq: number;
}

export interface RuntimeListView {
  schemaVersion: number;
  items: RuntimeRunView[];
  activeCount: number;
  maxRuns: number;
  note: string;
}

export interface RuntimeNotify {
  runId: string;
  seq: number;
  kind: "event" | "state" | "permission";
  interactionState: InteractionStatus;
  executionState: ExecutionStatus;
}

export interface RuntimeStartInput {
  installationId: string;
  cwd: string;
  prompt?: string;
  title?: string;
  permissionTimeoutMs?: number;
  model?: string;
  effort?: string;
}

export interface RuntimeAnswerInput {
  runId: string;
  permissionId: string;
  optionId: string;
}

export interface RuntimeReadQuery {
  runId: string;
  cursor?: number | null;
  maxEvents?: number;
}

export interface RuntimeResumeInput {
  sessionId: string;
  installationId?: string;
  cwd?: string;
  prompt?: string;
  permissionTimeoutMs?: number;
}


// ---------------------------------------------------------------------------
// S06 usage, quota pools and custom CLIs
// ---------------------------------------------------------------------------

export type UsageStatus =
  | "ok"
  | "partial"
  | "stale"
  | "unauthenticated"
  | "unsupported"
  | "rate-limited"
  | "error"
  | "unknown";

export type UsageFreshness = "fresh" | "stale" | "unknown";
export type UsageIdentityStatus = "confirmed" | "unconfirmed" | "unknown";
export type UsageSourceKind =
  | "official-machine"
  | "local-transcript"
  | "manual-entry"
  | "unavailable";
export type UsageMetric =
  | "quota-window"
  | "token-usage"
  | "credit-balance"
  | "cost-estimate"
  | "billing";
export type UsageUnit = "percent" | "tokens" | "credits" | "usd-micros" | "count";
export type UsageWindow =
  | "short"
  | "daily"
  | "weekly"
  | "monthly"
  | "lifetime"
  | "session"
  | "unspecified";

export interface UsageSample {
  metric: UsageMetric;
  unit: UsageUnit;
  window: UsageWindow;
  label: string;
  /** A missing number stays `null`; it is never rendered as 0. */
  usedPercent: number | null;
  remainingPercent: number | null;
  amount: number | null;
  totalTokens: number | null;
  inputTokens: number | null;
  cachedInputTokens: number | null;
  cacheCreationTokens: number | null;
  outputTokens: number | null;
  tokenSemantics: "input-includes-cached" | "input-excludes-cached" | null;
  resetsAt: string | null;
  windowMinutes: number | null;
  estimated: boolean;
  source: string;
  note: string | null;
}

export interface UsageSnapshot {
  id: string;
  poolId: string;
  agentId: string;
  status: UsageStatus;
  freshness: UsageFreshness;
  sourceKind: UsageSourceKind;
  source: string;
  capturedAt: string;
  expiresAt: string | null;
  error: AppError | null;
  retryAfterMs: number | null;
  nextAttemptAt: string | null;
  attempts: number;
  samples: UsageSample[];
  note: string | null;
  unavailableFields: string[];
}

export interface AccountProfile {
  id: string;
  hostId: string;
  agentId: string;
  product: string;
  vendor: string;
  label: string;
  identityStatus: UsageIdentityStatus;
  identityFingerprint: string | null;
  plan: string | null;
  source: string;
  observedAt: string;
}

export interface QuotaPool {
  id: string;
  poolKey: string;
  hostId: string;
  agentId: string;
  product: string;
  accountProfileId: string | null;
  identityStatus: UsageIdentityStatus;
  label: string;
  installationCount: number;
  createdAt: string;
  updatedAt: string;
}

export interface PoolView {
  pool: QuotaPool;
  snapshot: UsageSnapshot;
  lastGood: UsageSnapshot | null;
  installationIds: string[];
}

export interface UsageAccountView {
  account: AccountProfile;
  pools: PoolView[];
}

export interface ProviderCatalogEntry {
  agentId: string;
  product: string;
  /** `machine | transcript | manual | none` */
  quotaAccess: string;
  tokenAccess: string;
  officialEntry: string | null;
  status: string;
  /** Derived from `presence`; kept for existing consumers. */
  installed: boolean;
  /** `installed | not-installed | undiscovered` */
  presence: InstallPresence;
  evidence: string;
  limitation: string | null;
}

export type InstallPresence = "installed" | "not-installed" | "undiscovered";

export interface CatalogScanView {
  scannedRoots: number;
  entriesExamined: number;
  unreadableRoots: number;
  truncated: boolean;
  /** `true` only when an absence may be stated as a fact. */
  conclusive: boolean;
  scannedAt: string;
}

export interface UsageOverviewView {
  schemaVersion: number;
  generatedAt: string;
  accounts: UsageAccountView[];
  unconfirmedPools: PoolView[];
  catalog: ProviderCatalogEntry[];
  catalogScan: CatalogScanView;
  lastAttemptAt: string | null;
  lastSuccessAt: string | null;
  note: string;
}

export interface RefreshOutcome {
  agentId: string;
  status: UsageStatus;
  skipped: boolean;
  /** Stable code the UI translates; `reason` stays for details/logs. */
  reasonCode: string | null;
  reason: string | null;
  retryAfterMs: number | null;
}

export interface UsageRefreshResult {
  overview: UsageOverviewView;
  outcomes: RefreshOutcome[];
}

export interface BackupManifest {
  manifestVersion: number;
  createdAt: string;
  appVersion: string;
  profile: string;
  databaseSchema: number;
  fileName: string;
  sizeBytes: number;
  sha256: string;
  source: string;
  tables: string[];
  scopeNote: string;
}

export interface BackupRecord {
  manifest: BackupManifest;
  path: string;
  /** `ok | missing | checksum-mismatch | unreadable | invalid-name` */
  integrity: string;
}

export interface PendingRestore {
  /** Stable id of this concrete plan; may be absent in pre-R1 markers. */
  planId?: string;
  backupFileName: string;
  sha256: string;
  requestedAt: string;
  databaseSchema: number;
  note: string;
  /** True when the last start could not apply this restore. */
  failed?: boolean;
}

export interface RestoreOutcome {
  planId?: string;
  restoredFrom: string;
  restoredSha256?: string;
  appliedAt: string;
  previousCopy: string | null;
  databaseSchema: number;
}

export interface ProfileDataView {
  profile: string;
  dataRoot: string;
  databaseFile: string;
  databaseBytes: number;
  databaseSchema: number;
  expectedSchema: number;
  journalMode: string;
  backupDirectory: string;
  backups: BackupRecord[];
  pendingRestore: PendingRestore | null;
  lastRestore: RestoreOutcome | null;
  scopeNote: string;
}

export interface CustomCliInput {
  label: string;
  executable: string;
  args: string[];
  cwd?: string | null;
  productHint?: string | null;
}

export interface CustomCliView {
  id: string;
  label: string;
  executable: HostPath;
  args: string[];
  cwd: string | null;
  productHint: string | null;
  launchChain: TerminalLaunchChain;
  /** Always `unsupported`: a user-defined command has no product identity. */
  historyAccess: string;
  quotaAccess: string;
  verification: string;
  createdAt: string;
}
