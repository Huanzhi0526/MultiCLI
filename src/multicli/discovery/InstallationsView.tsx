import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { MultiCliApi } from "../api";
import { Badge, EmptyPanel, ErrorPanel, LoadingPanel } from "../components";
import { usePagedResource } from "../usePagedResource";
import type { InstallStatus, InstallationDiscoveryView } from "../types";
import { DISCOVERY_AGENTS, Field, ReadOnlyNote, ScanStatusBar } from "./controls";
import { useDiscoveryScan } from "./useDiscoveryScan";

const statusTone: Record<InstallStatus, "green" | "amber" | "red" | "neutral"> = {
  installed: "green",
  unverified: "amber",
  "missing-target": "red",
  "historical-residue": "neutral",
};

const probeTone: Record<string, "green" | "amber" | "red" | "neutral"> = {
  ok: "green",
  failed: "red",
  timeout: "amber",
  skipped: "neutral",
  "not-attempted": "neutral",
};

function installationIdOf(item: InstallationDiscoveryView): string {
  return [
    item.agentId,
    item.installationId ?? "",
    item.executable?.displayPath ?? "",
    item.status,
  ].join("|");
}

export default function InstallationsView({ api }: { api: MultiCliApi }) {
  const { t } = useTranslation("multicli");
  const { scan, busy, error, settings, settingsError, refresh, changeInterval } =
    useDiscoveryScan(api);
  const [agentFilter, setAgentFilter] = useState<string | null>(null);
  const installations = usePagedResource<InstallationDiscoveryView>(
    `installations|${agentFilter ?? "*"}`,
    (cursor) =>
      api.listInstallations({ limit: 100, cursor, agentId: agentFilter }),
    installationIdOf,
  );
  const reloadRows = useRef(installations.refresh);
  reloadRows.current = installations.refresh;
  const generation = scan?.generation ?? 0;
  useEffect(() => {
    if (generation > 0) {
      reloadRows.current();
    }
  }, [generation]);
  useEffect(() => {
    // Installations change rarely; a slow poll keeps the status bar honest
    // without re-probing versions (the backend caches probes for 5 minutes).
    const timer = setInterval(() => void refresh(), 30_000);
    return () => clearInterval(timer);
  }, [refresh]);

  return (
    <div className="flex min-h-0 flex-1 flex-col" data-testid="installations-view">
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
        <h2 className="text-sm font-semibold">{t("installationsTitle")}</h2>
        <span className="text-[11px] text-text-tertiary" data-testid="installations-count">
          {t("listCount", { items: installations.items.length, total: installations.total })}
        </span>
        <label className="ml-auto flex items-center gap-1 text-[11px] text-text-tertiary">
          {t("agentFilter")}
          <select
            className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 text-[11px] text-text-primary"
            value={agentFilter ?? ""}
            onChange={(event) =>
              setAgentFilter(event.target.value === "" ? null : event.target.value)
            }
            data-testid="installations-agent-filter"
          >
            <option value="">{t("filterAll")}</option>
            {DISCOVERY_AGENTS.map((agent) => (
              <option key={agent} value={agent}>
                {agent}
              </option>
            ))}
          </select>
        </label>
        <button
          type="button"
          className="rounded-md border border-separator-border px-3 py-1 text-[11px] text-text-secondary hover:bg-background-primary-hover"
          onClick={() => void refresh()}
          data-testid="installations-refresh"
        >
          {t("refresh")}
        </button>
      </div>
      <ReadOnlyNote />

      <div className="min-h-0 flex-1 overflow-auto px-5 pb-6">
        {installations.status === "loading" ? (
          <LoadingPanel label={t("loading")} testId="installations-loading" />
        ) : null}
        {installations.status === "error" && installations.error ? (
          <ErrorPanel
            error={installations.error}
            onRetry={installations.refresh}
            scope="installations"
          />
        ) : null}
        {installations.status === "ready" && installations.items.length === 0 ? (
          <EmptyPanel
            title={t("installationsEmpty")}
            hint={t("installationsEmptyHint")}
            testId="installations-empty"
          />
        ) : null}
        <ul className="flex flex-col gap-2">
          {installations.items.map((item, index) => (
            <li
              key={installationIdOf(item)}
              className="rounded-lg border border-separator-border px-4 py-3"
              data-testid={`installation-row-${item.agentId}-${index}`}
            >
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-sm font-medium text-text-primary">{item.product}</span>
                <code className="text-[11px] text-text-tertiary">{item.agentId}</code>
                <Badge
                  tone={statusTone[item.status]}
                  testId={`installation-status-${item.agentId}-${index}`}
                >
                  {t(`installStatus.${item.status}`)}
                </Badge>
                <Badge tone="blue">{t(`entryKind.${item.entryKind}`)}</Badge>
                {item.configProfileStatus === "unconfirmed" ? (
                  <Badge tone="amber" title={item.configProfileId}>
                    {t("profileUnconfirmed")}
                  </Badge>
                ) : null}
                <code className="ml-auto text-[10px] text-text-tertiary">
                  {item.signatureId}
                </code>
              </div>

              <p className="mt-1 font-mono text-[11px] leading-4 break-all text-text-secondary">
                {item.executable?.displayPath ?? t("notAvailable")}
              </p>
              {item.entryScript ? (
                <p className="mt-0.5 font-mono text-[10px] leading-4 break-all text-text-tertiary">
                  {t("entryScript")}: {item.entryScript.displayPath}
                </p>
              ) : null}
              {item.runtime ? (
                <p className="mt-0.5 font-mono text-[10px] leading-4 break-all text-text-tertiary">
                  {t("runtime")}: {item.runtime.displayPath}
                </p>
              ) : null}

              <dl className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-4">
                <Field label={t("version")}>
                  <span data-testid={`installation-version-${item.agentId}-${index}`}>
                    {item.version ?? t("notAvailable")}
                  </span>
                </Field>
                <Field label={t("probeStatus")}>
                  <span className="flex flex-wrap items-center gap-1">
                    <Badge tone={probeTone[item.probe.status] ?? "neutral"}>
                      {t(`probeStatusValue.${item.probe.status}`)}
                    </Badge>
                    <span className="text-[10px] text-text-tertiary">
                      {item.probe.checkedAt ?? t("notAvailable")}
                    </span>
                  </span>
                </Field>
                <Field label={t("configProfile")}>
                  <code className="text-[10px]">{item.configProfileId}</code>
                </Field>
                <Field label={t("lastSeenScan")}>
                  <span>#{item.lastSeenScan}</span>
                </Field>
              </dl>
              {item.probe.reason ? (
                <p className="mt-1 text-[10px] leading-4 text-text-tertiary">
                  {t("probeReason")}: {item.probe.reason}
                </p>
              ) : null}
              {item.issueReason ? (
                <p className="mt-1 text-[10px] leading-4 text-state-warning-text">
                  {item.issueCode}: {item.issueReason}
                </p>
              ) : null}
              <p className="mt-1 text-[10px] text-text-tertiary">
                {t("discoveredFrom")}: {item.discoveredFrom} · {t("discoveredAt")}:{" "}
                {item.discoveredAt}
              </p>
            </li>
          ))}
        </ul>
        {installations.status === "ready" && installations.cursor ? (
          <button
            type="button"
            className="mt-2 w-full rounded-md border border-separator-border px-3 py-1.5 text-xs text-text-secondary hover:bg-background-primary-hover"
            onClick={installations.loadMore}
            data-testid="installations-load-more"
          >
            {t("loadMore")}
          </button>
        ) : null}
        {installations.status === "loading-more" ? (
          <LoadingPanel label={t("loading")} />
        ) : null}
        {installations.status === "error-more" && installations.error ? (
          <ErrorPanel
            error={installations.error}
            onRetry={installations.loadMore}
            scope="installations-more"
            compact
          />
        ) : null}
      </div>
    </div>
  );
}
