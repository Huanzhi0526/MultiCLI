import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { cx } from "@/utils/cx";
import {
  isNotFound,
  messageText,
  summarizeCapabilities,
} from "./navigation";
import type { PagedResource, DetailResource } from "./usePagedResource";
import type {
  AgentListItem,
  AppError,
  Capability,
  SessionDetail,
  SessionOrigin,
  SessionSummary,
  WorkspaceListItem,
} from "./types";

type Tone = "neutral" | "blue" | "green" | "amber" | "red";

const toneClass: Record<Tone, string> = {
  neutral: "border-separator-border bg-background-secondary-default text-text-secondary",
  blue: "border-accent-200 bg-accent-50 text-accent-700",
  green: "border-state-success-base bg-state-success-base/30 text-state-success-text",
  amber: "border-orange-200 bg-orange-100 text-orange-800",
  red: "border-red-200 bg-red-100 text-red-700",
};

export function Badge({
  tone = "neutral",
  children,
  testId,
  title,
}: {
  tone?: Tone;
  children: ReactNode;
  testId?: string;
  title?: string;
}) {
  return (
    <span
      className={cx(
        "inline-flex items-center rounded-full border px-2 py-0.5 text-[11px] leading-4 font-medium whitespace-nowrap",
        toneClass[tone],
      )}
      data-testid={testId}
      title={title}
    >
      {children}
    </span>
  );
}

export function originTone(origin: SessionOrigin): Tone {
  if (origin === "managed") return "blue";
  if (origin === "external-observed") return "amber";
  return "neutral";
}

export function statusTone(status: Capability["status"]): Tone {
  if (status === "supported") return "green";
  if (status === "unverified") return "amber";
  return "red";
}

export function evidenceTone(level: Capability["evidenceLevel"]): Tone {
  if (level === "live") return "green";
  if (level === "fixture") return "amber";
  return "blue";
}

export function LoadingPanel({ label, testId }: { label: string; testId?: string }) {
  return (
    <div
      className="flex items-center gap-2 px-4 py-6 text-sm text-text-secondary"
      data-testid={testId}
      role="status"
    >
      <span className="size-3 animate-spin rounded-full border-2 border-separator-border border-t-accent-500" />
      {label}
    </div>
  );
}

export function EmptyPanel({
  title,
  hint,
  testId,
}: {
  title: string;
  hint?: string;
  testId?: string;
}) {
  return (
    <div
      className="flex flex-col items-center justify-center gap-2 px-6 py-12 text-center"
      data-testid={testId}
    >
      <p className="text-sm font-medium text-text-primary">{title}</p>
      {hint ? <p className="max-w-md text-xs leading-5 text-text-secondary">{hint}</p> : null}
    </div>
  );
}

export function ErrorPanel({
  error,
  onRetry,
  scope,
  compact = false,
}: {
  error: AppError;
  onRetry?: () => void;
  scope: string;
  compact?: boolean;
}) {
  const { t } = useTranslation("multicli");
  const details = Object.keys(error.details).length > 0
    ? JSON.stringify(error.details, null, 2)
    : "";
  return (
    <div
      className={cx(
        "flex min-w-0 flex-col gap-2 rounded-lg border border-border-error-default/40 bg-background-tertiary-error px-4",
        compact ? "py-3" : "py-5",
      )}
      data-testid={`${scope}-error`}
      role="alert"
    >
      <p className="text-sm font-medium text-text-error-primary">
        {t("errorTitle")}{" "}
        <code className="font-mono text-xs" data-testid={`${scope}-error-code`}>
          {error.code}
        </code>
      </p>
      <p className="text-xs leading-5 text-text-secondary [overflow-wrap:anywhere]">
        {error.message}
      </p>
      <p className="text-[11px] text-text-tertiary">
        {error.retryable ? t("errorRetryable") : t("errorNotRetryable")}
      </p>
      {details ? (
        <details className="text-[11px] text-text-tertiary">
          <summary className="cursor-pointer">{t("details")}</summary>
          <pre className="mt-1 overflow-auto whitespace-pre-wrap">{details}</pre>
        </details>
      ) : null}
      {onRetry ? (
        <div>
          <button
            type="button"
            className="rounded-md border border-separator-border bg-background-primary-default px-3 py-1 text-xs hover:bg-background-primary-hover"
            onClick={onRetry}
            data-testid={`${scope}-retry`}
          >
            {t("retry")}
          </button>
        </div>
      ) : null}
    </div>
  );
}

export function EntryTabs({
  entry,
  onChange,
}: {
  entry: "workspaces" | "agents";
  onChange: (next: "workspaces" | "agents") => void;
}) {
  const { t } = useTranslation("multicli");
  const options = [
    { id: "workspaces" as const, label: t("workspaces") },
    { id: "agents" as const, label: t("agents") },
  ];
  return (
    <div className="flex gap-1 rounded-lg bg-background-secondary-default p-1" role="tablist">
      {options.map((option) => (
        <button
          key={option.id}
          type="button"
          role="tab"
          aria-selected={entry === option.id}
          data-testid={`entry-${option.id}`}
          className={cx(
            "flex-1 rounded-md px-3 py-1.5 text-xs font-medium",
            entry === option.id
              ? "bg-background-primary-default text-text-primary shadow-sm"
              : "text-text-secondary hover:text-text-primary",
          )}
          onClick={() => onChange(option.id)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

export function ScopeList({
  entry,
  workspaces,
  agents,
  selectedId,
  onSelectAll,
  onSelectWorkspace,
  onSelectAgent,
  onRetry,
}: {
  entry: "workspaces" | "agents";
  workspaces: PagedResource<WorkspaceListItem>;
  agents: PagedResource<AgentListItem>;
  selectedId: string | null;
  onSelectAll: () => void;
  onSelectWorkspace: (id: string) => void;
  onSelectAgent: (id: string) => void;
  onRetry: () => void;
}) {
  const { t } = useTranslation("multicli");
  const current = entry === "workspaces" ? workspaces : agents;
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <button
        type="button"
        className={cx(
          "mx-3 mt-3 rounded-lg border px-3 py-2 text-left text-xs",
          selectedId === null
            ? "border-accent-300 bg-accent-50 text-accent-700"
            : "border-separator-border text-text-secondary hover:bg-background-primary-hover",
        )}
        onClick={onSelectAll}
        data-testid="scope-all"
      >
        {t("allSessions")}
      </button>
      <div className="mt-2 min-h-0 flex-1 overflow-auto px-3 pb-3">
        {current.status === "loading" ? (
          <LoadingPanel label={t("loading")} testId={`${entry}-loading`} />
        ) : null}
        {current.status === "error" && current.error ? (
          <ErrorPanel
            error={current.error}
            onRetry={onRetry}
            scope={`${entry}-list`}
            compact
          />
        ) : null}
        {current.status === "ready" && current.items.length === 0 ? (
          <EmptyPanel
            title={entry === "workspaces" ? t("emptyWorkspaces") : t("emptyAgents")}
            hint={t("emptyScopeHint")}
            testId={`empty-${entry}`}
          />
        ) : null}
        <ul className="flex flex-col gap-1">
          {entry === "workspaces"
            ? workspaces.items.map((item) => (
                <li key={item.workspace.id}>
                  <button
                    type="button"
                    className={cx(
                      "w-full rounded-lg px-3 py-2 text-left",
                      selectedId === item.workspace.id
                        ? "bg-background-tertiary-default"
                        : "hover:bg-background-primary-hover",
                    )}
                    onClick={() => onSelectWorkspace(item.workspace.id)}
                    data-testid={`workspace-row-${item.workspace.id}`}
                  >
                    <span
                      className="block truncate text-xs font-medium text-text-primary"
                      title={item.workspace.path.displayPath}
                    >
                      {item.workspace.path.displayPath}
                    </span>
                    <span className="mt-1 flex flex-wrap items-center gap-1 text-[11px] text-text-tertiary">
                      <span>{t("sessionCount", { count: item.sessionCount })}</span>
                      {item.workspace.repoGroupId ? (
                        <Badge tone="blue">{t("repoGroup")}</Badge>
                      ) : null}
                      {item.workspace.path.canonicalPath === null ? (
                        <Badge tone="amber">{t("pathUnresolved")}</Badge>
                      ) : null}
                    </span>
                  </button>
                </li>
              ))
            : null}
          {entry === "agents"
            ? agents.items.map((item) => (
                <li key={item.agent.id}>
                  <button
                    type="button"
                    className={cx(
                      "w-full rounded-lg px-3 py-2 text-left",
                      selectedId === item.agent.id
                        ? "bg-background-tertiary-default"
                        : "hover:bg-background-primary-hover",
                    )}
                    onClick={() => onSelectAgent(item.agent.id)}
                    data-testid={`agent-row-${item.agent.id}`}
                  >
                    <span className="flex items-center justify-between gap-2">
                      <span className="truncate text-xs font-medium text-text-primary">
                        {item.agent.displayName}
                      </span>
                      <code className="text-[10px] text-text-tertiary">{item.agent.id}</code>
                    </span>
                    <span className="mt-1 flex flex-wrap gap-2 text-[11px] text-text-tertiary">
                      <span>{t("sessionCount", { count: item.sessionCount })}</span>
                      <span>{t("installationCount", { count: item.installationCount })}</span>
                    </span>
                  </button>
                </li>
              ))
            : null}
        </ul>
        {current.status === "ready" && current.cursor ? (
          <button
            type="button"
            className="mt-2 w-full rounded-md border border-separator-border px-3 py-1.5 text-xs text-text-secondary hover:bg-background-primary-hover"
            onClick={current.loadMore}
            data-testid={`${entry}-load-more`}
          >
            {t("loadMore")}
          </button>
        ) : null}
        {current.status === "loading-more" ? (
          <LoadingPanel label={t("loading")} />
        ) : null}
        {current.status === "error-more" && current.error ? (
          <ErrorPanel
            error={current.error}
            onRetry={current.loadMore}
            scope={`${entry}-more`}
            compact
          />
        ) : null}
      </div>
    </div>
  );
}

export function SessionList({
  sessions,
  selectedId,
  origin,
  onOriginChange,
  onSelect,
  onRefresh,
  onRetry,
}: {
  sessions: PagedResource<SessionSummary>;
  selectedId: string | null;
  origin: SessionOrigin | null;
  onOriginChange: (next: SessionOrigin | null) => void;
  onSelect: (id: string) => void;
  onRefresh: () => void;
  onRetry: () => void;
}) {
  const { t } = useTranslation("multicli");
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-2 border-b border-separator-border px-4 py-3">
        <h2 className="text-sm font-semibold">{t("sessions")}</h2>
        <span className="text-[11px] text-text-tertiary" data-testid="session-list-count">
          {sessions.items.length > 0
            ? t("listCount", { items: sessions.items.length, total: sessions.total })
            : ""}
        </span>
        <button
          type="button"
          className="ml-auto rounded-md border border-separator-border px-2 py-1 text-[11px] text-text-secondary hover:bg-background-primary-hover disabled:opacity-50"
          onClick={onRefresh}
          disabled={sessions.status === "loading" || sessions.status === "loading-more"}
          data-testid="sessions-refresh"
        >
          {t("refresh")}
        </button>
      </div>
      <div className="flex items-center gap-2 border-b border-separator-border px-4 py-2">
        <label className="text-[11px] text-text-tertiary" htmlFor="origin-filter">
          {t("origin")}
        </label>
        <select
          id="origin-filter"
          className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 text-xs"
          value={origin ?? ""}
          onChange={(event) => {
            const value = event.target.value;
            onOriginChange(value === "" ? null : (value as SessionOrigin));
          }}
          data-testid="origin-filter"
        >
          <option value="">{t("originAll")}</option>
          <option value="managed">{t("originManaged")}</option>
          <option value="external-observed">{t("originExternal")}</option>
          <option value="history-only">{t("originHistory")}</option>
        </select>
      </div>
      <div className="min-h-0 flex-1 overflow-auto px-3 py-3">
        {sessions.status === "loading" ? (
          <LoadingPanel label={t("loading")} testId="sessions-loading" />
        ) : null}
        {sessions.status === "error" && sessions.error ? (
          <ErrorPanel
            error={sessions.error}
            onRetry={onRetry}
            scope="sessions"
          />
        ) : null}
        {sessions.status === "ready" && sessions.items.length === 0 ? (
          <EmptyPanel
            title={t("emptySessions")}
            hint={t("emptySessionsHint")}
            testId="empty-sessions"
          />
        ) : null}
        <ul className="flex flex-col gap-1">
          {sessions.items.map((item) => (
            <li key={item.session.id}>
              <button
                type="button"
                aria-current={selectedId === item.session.id ? "true" : undefined}
                className={cx(
                  "w-full rounded-lg border px-3 py-2 text-left",
                  selectedId === item.session.id
                    ? "border-accent-300 bg-accent-50"
                    : "border-transparent hover:bg-background-primary-hover",
                )}
                onClick={() => onSelect(item.session.id)}
                data-testid={`session-row-${item.session.id}`}
              >
                <span className="block truncate text-xs font-medium text-text-primary">
                  {item.session.title || t("untitledSession")}
                </span>
                <span className="mt-1 flex flex-wrap items-center gap-1.5 text-[11px] text-text-tertiary">
                  <Badge tone={originTone(item.session.origin)}>
                    {t(`origin.${item.session.origin}`)}
                  </Badge>
                  <code>{item.session.agentId}</code>
                  <span>{t("messageCount", { count: item.messageCount })}</span>
                  <span>{t("runCount", { count: item.runCount })}</span>
                  <span>{t("workspaceCount", { count: item.workspaceIds.length })}</span>
                </span>
              </button>
            </li>
          ))}
        </ul>
        {sessions.status === "loading-more" ? <LoadingPanel label={t("loading")} /> : null}
        {sessions.status === "error-more" && sessions.error ? (
          <ErrorPanel
            error={sessions.error}
            onRetry={sessions.loadMore}
            scope="sessions-more"
            compact
          />
        ) : null}
        {sessions.status === "ready" && sessions.cursor ? (
          <button
            type="button"
            className="mt-2 w-full rounded-md border border-separator-border px-3 py-1.5 text-xs text-text-secondary hover:bg-background-primary-hover"
            onClick={sessions.loadMore}
            data-testid="sessions-load-more"
          >
            {t("loadMore")}
          </button>
        ) : null}
      </div>
    </div>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-0.5">
      <dt className="text-[10px] uppercase tracking-wide text-text-tertiary">{label}</dt>
      <dd className="text-xs text-text-primary">{children}</dd>
    </div>
  );
}

function CapabilitySection({
  capabilities,
}: {
  capabilities: PagedResource<Capability>;
}) {
  const { t } = useTranslation("multicli");
  const summary = summarizeCapabilities(capabilities.items);
  return (
    <section className="border-t border-separator-border px-5 py-4">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-text-secondary">
          {t("capabilities")}
        </h3>
        {capabilities.items.length > 0 ? (
          <>
            <Badge tone="green">
              {t("capabilitySummary.supported", { count: summary.supported })}
            </Badge>
            <Badge tone="red">
              {t("capabilitySummary.unsupported", { count: summary.unsupported })}
            </Badge>
            <Badge tone="amber">
              {t("capabilitySummary.unverified", { count: summary.unverified })}
            </Badge>
            <Badge tone="blue">
              {t("capabilitySummary.document", { count: summary.document })}
            </Badge>
            <Badge tone="amber">
              {t("capabilitySummary.fixture", { count: summary.fixture })}
            </Badge>
            <Badge tone="green">{t("capabilitySummary.live", { count: summary.live })}</Badge>
          </>
        ) : null}
      </div>
      {capabilities.status === "loading" ? (
        <LoadingPanel label={t("loading")} testId="capabilities-loading" />
      ) : null}
      {capabilities.status === "error" && capabilities.error ? (
        <div className="mt-2">
          <ErrorPanel
            error={capabilities.error}
            onRetry={capabilities.refresh}
            scope="capabilities"
            compact
          />
        </div>
      ) : null}
      {capabilities.status === "ready" && capabilities.items.length === 0 ? (
        <p className="mt-2 text-xs text-text-tertiary">{t("noCapabilities")}</p>
      ) : null}
      <ul className="mt-2 flex flex-col gap-2">
        {capabilities.items.map((capability) => (
          <li
            key={`${capability.agentId}|${capability.name}|${capability.installationId ?? ""}`}
            className="rounded-lg border border-separator-border px-3 py-2"
            data-testid={`capability-row-${capability.name}`}
          >
            <div className="flex flex-wrap items-center gap-1.5">
              <code className="text-xs font-medium">{capability.name}</code>
              <Badge tone={statusTone(capability.status)}>
                {t(`capabilityStatus.${capability.status}`)}
              </Badge>
              <Badge tone={evidenceTone(capability.evidenceLevel)}>
                {t(`capabilityEvidence.${capability.evidenceLevel}`)}
              </Badge>
              <span className="text-[10px] text-text-tertiary">
                {t("versionRange")}: {capability.versionRange ?? t("notAvailable")}
              </span>
            </div>
            <p className="mt-1 text-[11px] leading-4 text-text-secondary">
              {capability.reason ?? t("noReason")}
            </p>
            <p className="mt-0.5 text-[10px] text-text-tertiary">
              {t("source")}: {capability.source} · {t("verifiedAt")}:{" "}
              {capability.verifiedAt ?? t("notVerified")}
            </p>
          </li>
        ))}
      </ul>
      {capabilities.status === "ready" && capabilities.cursor ? (
        <button
          type="button"
          className="mt-2 rounded-md border border-separator-border px-3 py-1 text-[11px] text-text-secondary hover:bg-background-primary-hover"
          onClick={capabilities.loadMore}
          data-testid="capabilities-load-more"
        >
          {t("loadMore")}
        </button>
      ) : null}
      {capabilities.status === "loading-more" ? (
        <LoadingPanel label={t("loading")} />
      ) : null}
      {capabilities.status === "error-more" && capabilities.error ? (
        <ErrorPanel
          error={capabilities.error}
          onRetry={capabilities.loadMore}
          scope="capabilities-more"
          compact
        />
      ) : null}
    </section>
  );
}

export function SessionDetailView({
  detail,
  capabilities,
  onRetry,
}: {
  detail: DetailResource<SessionDetail>;
  capabilities: PagedResource<Capability>;
  onRetry: () => void;
}) {
  const { t } = useTranslation("multicli");
  if (detail.status === "idle") {
    return (
      <EmptyPanel
        title={t("detailEmpty")}
        hint={t("detailEmptyHint")}
        testId="detail-empty"
      />
    );
  }
  if (detail.status === "loading") {
    return <LoadingPanel label={t("loading")} testId="detail-loading" />;
  }
  if (detail.status === "error" && detail.error) {
    if (isNotFound(detail.error)) {
      return (
        <div className="p-5" data-testid="detail-not-found" role="alert">
          <EmptyPanel
            title={t("sessionMissing")}
            hint={t("sessionMissingHint")}
          />
          <p className="text-center text-[11px] text-text-tertiary">
            {detail.error.code}: {detail.error.message}
          </p>
          <div className="mt-2 text-center">
            <button
              type="button"
              className="rounded-md border border-separator-border px-3 py-1 text-xs"
              onClick={onRetry}
              data-testid="detail-retry"
            >
              {t("retry")}
            </button>
          </div>
        </div>
      );
    }
    return (
      <div className="p-5">
        <ErrorPanel error={detail.error} onRetry={onRetry} scope="detail" />
      </div>
    );
  }
  const data = detail.data;
  if (!data) {
    return <EmptyPanel title={t("detailEmpty")} testId="detail-empty" />;
  }
  const session = data.session;
  return (
    <article className="flex flex-col" data-testid="session-detail">
      <header className="border-b border-separator-border px-5 py-4">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="text-base font-semibold" data-testid="detail-title">
            {session.title || t("untitledSession")}
          </h2>
          <Badge tone={originTone(session.origin)}>{t(`origin.${session.origin}`)}</Badge>
        </div>
        <p className="mt-1 text-[11px] text-text-tertiary">
          <code data-testid="detail-session-id">{session.id}</code> ·{" "}
          {t("agent")}: <code>{session.agentId}</code> · {t("host")}:{" "}
          <code>{session.hostId}</code>
        </p>
        <dl className="mt-3 grid grid-cols-2 gap-3 sm:grid-cols-4">
          <Field label={t("messageCountLabel")}>
            <span data-testid="detail-count-messages">{data.messageCount}</span>
          </Field>
          <Field label={t("runCountLabel")}>
            <span data-testid="detail-count-runs">{data.runCount}</span>
          </Field>
          <Field label={t("nativeId")}>
            {session.nativeId ?? t("notAvailable")}
          </Field>
          <Field label={t("sourceNamespace")}>
            <code className="break-all">{session.sourceNamespace}</code>
          </Field>
        </dl>
      </header>

      <section className="border-b border-separator-border px-5 py-4">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-text-secondary">
          {t("workspacesOfSession")}
        </h3>
        <ul className="mt-2 flex flex-col gap-2">
          {data.workspaces.map((link) => (
            <li key={link.workspaceId} className="rounded-lg border border-separator-border px-3 py-2">
              <div className="flex items-center gap-2">
                <Badge tone={link.role === "primary" ? "blue" : "neutral"}>
                  {link.role === "primary" ? t("primary") : t("related")}
                </Badge>
                <code className="text-[11px] break-all">{link.workspaceId}</code>
              </div>
              <p className="mt-1 text-[11px] text-text-tertiary">
                {t("evidenceOfLink")}: {link.evidence}
              </p>
            </li>
          ))}
        </ul>
      </section>

      <section className="border-b border-separator-border px-5 py-4">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-text-secondary">
          {t("runs")}
        </h3>
        {data.runs.length === 0 ? (
          <p className="mt-2 text-xs text-text-tertiary">{t("noRuns")}</p>
        ) : (
        <div className="mt-2 overflow-auto">
          <table className="w-full min-w-[680px] border-collapse text-left text-[11px]">
            <thead className="text-text-tertiary">
              <tr>
                <th className="border-b border-separator-border py-1 pr-2 font-medium">id</th>
                <th className="border-b border-separator-border py-1 pr-2 font-medium">
                  {t("transport")}
                </th>
                <th className="border-b border-separator-border py-1 pr-2 font-medium">
                  {t("ownership")}
                </th>
                <th className="border-b border-separator-border py-1 pr-2 font-medium">
                  {t("executionStatus")}
                </th>
                <th className="border-b border-separator-border py-1 pr-2 font-medium">
                  {t("interactionStatus")}
                </th>
                <th className="border-b border-separator-border py-1 pr-2 font-medium">
                  {t("evidence")}
                </th>
                <th className="border-b border-separator-border py-1 font-medium">
                  {t("startedAt")}
                </th>
                <th className="border-b border-separator-border py-1 font-medium">
                  {t("endedAt")}
                </th>
              </tr>
            </thead>
            <tbody>
              {data.runs.map((run) => (
                <tr key={run.id} data-testid={`run-row-${run.id}`}>
                  <td className="border-b border-separator-border py-1.5 whitespace-nowrap pr-2 font-mono">{run.id}</td>
                  <td className="border-b border-separator-border py-1.5 whitespace-nowrap pr-2">{run.transport}</td>
                  <td className="border-b border-separator-border py-1.5 whitespace-nowrap pr-2">{run.ownership}</td>
                  <td className="border-b border-separator-border py-1.5 whitespace-nowrap pr-2">
                    {run.executionStatus}
                  </td>
                  <td className="border-b border-separator-border py-1.5 whitespace-nowrap pr-2">
                    {run.interactionStatus}
                  </td>
                  <td className="border-b border-separator-border py-1.5 whitespace-nowrap pr-2">
                    {run.evidenceLevel}
                  </td>
                  <td className="border-b border-separator-border py-1.5 whitespace-nowrap pr-2">
                    {run.startedAt ?? t("notAvailable")}
                  </td>
                  <td className="border-b border-separator-border py-1.5 whitespace-nowrap">
                    {run.endedAt ?? t("notAvailable")}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        )}
      </section>

      <CapabilitySection capabilities={capabilities} />

      <section className="px-5 py-4">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-text-secondary">
          {t("messages")}
        </h3>
        {data.messages.length === 0 ? (
          <p className="mt-2 text-xs text-text-tertiary">{t("noMessages")}</p>
        ) : (
        <ol className="mt-2 flex flex-col gap-2">
          {data.messages.map((message) => (
            <li
              key={message.id}
              className="rounded-lg border border-separator-border px-3 py-2"
              data-testid={`message-row-${message.id}`}
            >
              <div className="flex flex-wrap items-center gap-2 text-[10px] text-text-tertiary">
                <span>{t("messageSeq", { seq: message.seq })}</span>
                <code>{message.kind}</code>
                <span>{message.observedAt}</span>
                {message.runId ? <code>{message.runId}</code> : null}
              </div>
              <p className="mt-1 whitespace-pre-wrap text-xs leading-5 text-text-primary">
                {messageText(message)}
              </p>
            </li>
          ))}
        </ol>
        )}
      </section>
    </article>
  );
}
