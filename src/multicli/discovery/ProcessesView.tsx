import { localTime } from "../presentation";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { MultiCliApi } from "../api";
import { Badge, EmptyPanel, ErrorPanel, LoadingPanel } from "../components";
import { usePagedResource } from "../usePagedResource";
import type { Freshness, ProcessDiscoveryView, ProcessState } from "../types";
import { DISCOVERY_AGENTS, Field, ReadOnlyNote, ScanStatusBar } from "./controls";
import { useDiscoveryScan } from "./useDiscoveryScan";

const stateTone: Record<ProcessState, "green" | "neutral" | "amber"> = {
  alive: "green",
  exited: "neutral",
  unknown: "amber",
};

const freshnessTone: Record<Freshness, "green" | "amber"> = {
  fresh: "green",
  stale: "amber",
};

function rowId(item: ProcessDiscoveryView): string {
  if (item.identity) {
    return `${item.identity.hostId}|${item.identity.pid}|${item.identity.creationTime}`;
  }
  return `candidate|${item.pid ?? 0}`;
}

export default function ProcessesView({ api }: { api: MultiCliApi }) {
  const { t } = useTranslation("multicli");
  const {
    scan,
    busy,
    error,
    settings,
    settingsError,
    refresh,
    changeInterval,
    applyScan,
  } = useDiscoveryScan(api);
  const [agentFilter, setAgentFilter] = useState<string | null>(null);
  const [stateFilter, setStateFilter] = useState<ProcessState | null>(null);
  const processes = usePagedResource<ProcessDiscoveryView>(
    `processes|${agentFilter ?? "*"}|${stateFilter ?? "*"}`,
    async (cursor) => {
      const page = await api.listProcesses({
        limit: 100,
        cursor,
        agentId: agentFilter,
        state: stateFilter,
      });
      // The same response carries the scan state, so the status bar never
      // drifts away from the rows.
      applyScan(page.scan);
      return { items: page.items, nextCursor: page.nextCursor, total: page.total };
    },
    rowId,
  );
  const reloadRows = useRef(processes.refresh);
  reloadRows.current = processes.refresh;
  const hasMore = processes.cursor !== null;
  const generation = scan?.generation ?? 0;

  // A new scan generation always replaces page 1. Auto refresh pauses while
  // the user has loaded further pages so a cursor never crosses generations.
  useEffect(() => {
    if (generation > 0) {
      reloadRows.current();
    }
  }, [generation]);
  useEffect(() => {
    if (hasMore) {
      return;
    }
    // Poll faster than the sampling interval so the list and the status bar
    // lag the backend scan by less than one interval. Each poll is a cheap
    // read of the in-memory snapshot; it never starts a scan.
    const intervalMs = Math.max(1000, Math.floor((scan?.intervalMs ?? 3000) / 2));
    const timer = setInterval(() => reloadRows.current(), intervalMs);
    return () => clearInterval(timer);
  }, [hasMore, scan?.intervalMs]);

  return (
    <div className="flex min-h-0 flex-1 flex-col" data-testid="processes-view">
      <ScanStatusBar
        scan={scan}
        busy={busy}
        error={error}
        settings={settings}
        settingsError={settingsError}
        onRefresh={() => void refresh()}
        onIntervalChange={(value) => void changeInterval(value)}
      />
      <div className="flex flex-wrap items-center gap-2 border-b border-separator-border px-5 py-2">
        <h2 className="text-sm font-semibold">{t("processesTitle")}</h2>
        <span className="text-[11px] text-text-tertiary" data-testid="processes-count">
          {t("listCount", { items: processes.items.length, total: processes.total })}
        </span>
        <label className="ml-auto flex items-center gap-1 text-[11px] text-text-tertiary">
          {t("agentFilter")}
          <select
            className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 text-[11px] text-text-primary"
            value={agentFilter ?? ""}
            onChange={(event) =>
              setAgentFilter(event.target.value === "" ? null : event.target.value)
            }
            data-testid="processes-agent-filter"
          >
            <option value="">{t("filterAll")}</option>
            {DISCOVERY_AGENTS.map((agent) => (
              <option key={agent} value={agent}>
                {agent}
              </option>
            ))}
          </select>
        </label>
        <label className="flex items-center gap-1 text-[11px] text-text-tertiary">
          {t("stateFilter")}
          <select
            className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 text-[11px] text-text-primary"
            value={stateFilter ?? ""}
            onChange={(event) =>
              setStateFilter(event.target.value === "" ? null : (event.target.value as ProcessState))
            }
            data-testid="processes-state-filter"
          >
            <option value="">{t("filterAll")}</option>
            <option value="alive">{t("processState.alive")}</option>
            <option value="exited">{t("processState.exited")}</option>
            <option value="unknown">{t("processState.unknown")}</option>
          </select>
        </label>
        <button
          type="button"
          className="rounded-md border border-separator-border px-3 py-1 text-[11px] text-text-secondary hover:bg-background-primary-hover"
          onClick={() => void refresh()}
          data-testid="processes-refresh"
        >
          {t("refresh")}
        </button>
      </div>
      <ReadOnlyNote />

      <div className="min-h-0 flex-1 overflow-auto px-5 pb-6">
        {processes.status === "loading" ? (
          <LoadingPanel label={t("loading")} testId="processes-loading" />
        ) : null}
        {processes.status === "error" && processes.error ? (
          <ErrorPanel
            error={processes.error}
            onRetry={processes.refresh}
            scope="processes"
          />
        ) : null}
        {processes.status === "ready" && processes.items.length === 0 ? (
          <EmptyPanel
            title={t("processesEmpty")}
            hint={t("processesEmptyHint")}
            testId="processes-empty"
          />
        ) : null}
        <ul className="flex flex-col gap-2">
          {processes.items.map((item) => (
            <li
              key={rowId(item)}
              className="rounded-lg border border-separator-border px-4 py-3"
              data-testid={`process-row-${item.identity?.pid ?? item.pid ?? "unknown"}`}
            >
              <div className="flex flex-wrap items-center gap-2">
                <Badge tone={stateTone[item.state]} testId={`process-state-${item.pid}`}>
                  {t(`processState.${item.state}`)}
                </Badge>
                <Badge tone={freshnessTone[item.freshness]}>
                  {t(`freshness.${item.freshness}`)}
                </Badge>
                <span className="text-sm font-medium text-text-primary">
                  {item.product ?? t("unknownProduct")}
                </span>
                <code className="text-[11px] text-text-tertiary">{item.agentId ?? "—"}</code>
                <code className="text-[11px] text-text-tertiary" data-testid="process-pid">
                  pid {item.pid ?? "—"}
                </code>
                {item.recognition ? (
                  <Badge tone={item.recognition === "confirmed" ? "blue" : "amber"}>
                    {t(`recognition.${item.recognition}`)}
                  </Badge>
                ) : null}
                {item.entryKind ? (
                  <Badge tone="neutral">{t(`entryKind.${item.entryKind}`)}</Badge>
                ) : null}
                {item.stateReason ? (
                  <code className="ml-auto text-[10px] text-text-tertiary">
                    {item.stateReason}
                  </code>
                ) : null}
              </div>

              {item.identity ? (
                <p className="mt-1 font-mono text-[10px] leading-4 break-all text-text-tertiary">
                  {t("processIdentity")}: {item.identity.hostId} · {item.identity.pid} ·{" "}
                  <span data-testid="process-creation-time">{item.identity.creationTime}</span>
                </p>
              ) : (
                <p className="mt-1 text-[10px] leading-4 text-state-warning-text">
                  {t("identityUnconfirmed")}
                </p>
              )}

              <dl className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-4">
                <Field label={t("sourceProgram")}>
                  <span data-testid="process-source-program">
                    {item.sourceProgram ?? t("notAvailable")}
                  </span>
                </Field>
                <Field label={t("parentProcess")}>
                  <span>
                    {item.parentIdentity
                      ? `pid ${item.parentIdentity.pid}`
                      : item.parentNote ?? t("parentUnlinked")}
                  </span>
                </Field>
                <Field label={t("workspaceLink")}>
                  <span data-testid="process-workspace">
                    {item.workspaceId ?? t("workspaceUnlinked")}
                  </span>
                </Field>
                <Field label={t("lastSeenScan")}>
                  <span>#{item.lastSeenScan}</span>
                </Field>
              </dl>

              {item.executable ? (
                <p className="mt-1 font-mono text-[10px] leading-4 break-all text-text-secondary">
                  {item.executable.displayPath}
                </p>
              ) : null}
              {item.redactedCommand ? (
                <p
                  className="mt-1 font-mono text-[10px] leading-4 break-all text-text-tertiary"
                  data-testid="process-command"
                >
                  {item.redactedCommand}
                </p>
              ) : null}
              <p className="mt-1 text-[10px] text-text-tertiary">
                {t("evidenceLabel")}: {item.evidence.join(", ") || t("notAvailable")}
                {item.workspaceEvidence ? ` · ${item.workspaceEvidence}` : ""}
              </p>
              <p className="mt-1 text-[10px] text-text-tertiary">
                {t("firstSeen")}: {localTime(item.firstSeenAt)} · {t("lastSeen")}: {localTime(item.lastSeenAt)}
              </p>
            </li>
          ))}
        </ul>
        {processes.status === "ready" && processes.cursor ? (
          <button
            type="button"
            className="mt-2 w-full rounded-md border border-separator-border px-3 py-1.5 text-xs text-text-secondary hover:bg-background-primary-hover"
            onClick={processes.loadMore}
            data-testid="processes-load-more"
          >
            {t("loadMore")}
          </button>
        ) : null}
        {processes.status === "loading-more" ? (
          <LoadingPanel label={t("loading")} />
        ) : null}
        {processes.status === "error-more" && processes.error ? (
          <ErrorPanel
            error={processes.error}
            onRetry={processes.loadMore}
            scope="processes-more"
            compact
          />
        ) : null}
      </div>
    </div>
  );
}
