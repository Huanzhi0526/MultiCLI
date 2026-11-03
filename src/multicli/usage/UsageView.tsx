import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { MultiCliApi } from "../api";
import { Badge, ErrorPanel, LoadingPanel } from "../components";
import type {
  AppError,
  PoolView,
  RefreshOutcome,
  UsageOverviewView,
  UsageSample,
  UsageSnapshot,
  UsageStatus,
} from "../types";
import { toAppError } from "../navigation";

type Tone = "green" | "amber" | "red" | "neutral" | "blue";
type Freshness = "fresh" | "stale" | "unknown";

/** Tone per reading state. A stale reading is amber even when the underlying
 *  status was `ok`, because the number is no longer current; a real failure
 *  stays red even when it is also old. */
function toneFor(status: UsageStatus, freshness: Freshness): Tone {
  if (
    freshness === "stale" &&
    (status === "ok" || status === "partial" || status === "stale")
  ) {
    return "amber";
  }
  const base: Record<UsageStatus, Tone> = {
    ok: "green",
    partial: "amber",
    stale: "amber",
    unauthenticated: "amber",
    unsupported: "neutral",
    "rate-limited": "red",
    error: "red",
    unknown: "neutral",
  };
  if (freshness === "unknown" && status === "ok") {
    return "neutral";
  }
  return base[status];
}

/**
 * Freshness is re-evaluated against the local clock instead of being frozen at
 * the moment the overview was fetched. A reading that passes its `expiresAt`
 * while the page stays open must stop looking current.
 *
 * It only ever downgrades: `stale`/`unknown` from the backend are kept, and a
 * clock that moved *backwards* cannot prove the reading is current either.
 */
function freshnessNow(snapshot: UsageSnapshot, now: number): Freshness {
  if (snapshot.freshness === "unknown") {
    return "unknown";
  }
  if (snapshot.freshness === "stale") {
    return "stale";
  }
  const captured = Date.parse(snapshot.capturedAt);
  if (!Number.isFinite(captured)) {
    return "unknown";
  }
  if (now + 60_000 < captured) {
    return "unknown";
  }
  const expires = snapshot.expiresAt ? Date.parse(snapshot.expiresAt) : Number.NaN;
  if (Number.isFinite(expires) && now >= expires) {
    return "stale";
  }
  return "fresh";
}

function localTime(value: string | null): string {
  if (!value) {
    return "—";
  }
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString();
}

function timeZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "local";
  } catch {
    return "local";
  }
}

function numberText(value: number | null): string {
  return value === null ? "—" : value.toLocaleString();
}

function percentText(value: number | null): string {
  return value === null ? "—" : `${value.toFixed(1)}%`;
}

/** Micro-dollars are the provider's unit; showing them raw is unreadable. */
function amountText(sample: UsageSample): string {
  if (sample.amount === null) {
    return "—";
  }
  if (sample.unit === "usd-micros") {
    const dollars = (sample.amount / 1_000_000).toFixed(2);
    return sample.estimated ? `≈ $${dollars}` : `$${dollars}`;
  }
  return sample.amount.toLocaleString();
}

const windowKey: Record<string, string> = {
  short: "usageWindowShort",
  daily: "usageWindowDaily",
  weekly: "usageWindowWeekly",
  monthly: "usageWindowMonthly",
  lifetime: "usageWindowLifetime",
  session: "usageWindowSession",
  unspecified: "usageWindowUnspecified",
};

function SampleRow({ sample }: { sample: UsageSample }) {
  const { t } = useTranslation("multicli");
  const value =
    sample.metric === "quota-window"
      ? `${percentText(sample.usedPercent)} / ${percentText(sample.remainingPercent)}`
      : sample.metric === "token-usage"
        ? numberText(sample.totalTokens)
        : amountText(sample);
  const detail =
    sample.metric === "token-usage"
      ? `${t("usageTokenDetail")}`
      : sample.resetsAt
        ? `${t("usageReset")}: ${localTime(sample.resetsAt)}`
        : "—";
  return (
    <tr className="border-t border-separator-border text-[11px]">
      <td className="py-1 pr-2 text-text-primary">{sample.label}</td>
      <td className="py-1 pr-2 text-text-secondary">
        {value}
        <span className="ml-1 text-[9px] text-text-tertiary">
          {sample.metric === "quota-window" ? "" : sample.unit}
        </span>
      </td>
      <td className="py-1 pr-2 text-text-tertiary">
        {sample.metric === "token-usage" || sample.metric === "quota-window"
          ? t(windowKey[sample.window] ?? "usageWindowUnspecified")
          : sample.unit}
        {sample.windowMinutes ? ` · ${sample.windowMinutes} min` : ""}
      </td>
      <td className="py-1 pr-2 text-text-tertiary" title={detail}>
        {sample.metric === "token-usage"
          ? `in ${numberText(sample.inputTokens)} · cached ${numberText(
              sample.cachedInputTokens,
            )} · out ${numberText(sample.outputTokens)}`
          : localTime(sample.resetsAt)}
      </td>
      <td className="py-1 text-text-tertiary">
        {sample.estimated ? (
          <Badge tone="amber">{t("usageEstimated")}</Badge>
        ) : sample.metric === "billing" ? (
          <Badge tone="blue">{t("usageBilling")}</Badge>
        ) : (
          sample.source
        )}
      </td>
    </tr>
  );
}

function SnapshotBody({
  snapshot,
  scope,
  clock,
}: {
  snapshot: UsageSnapshot;
  scope: string;
  clock: number;
}) {
  const { t } = useTranslation("multicli");
  const freshness = freshnessNow(snapshot, clock);
  return (
    <div className="mt-2">
      <div className="flex flex-wrap items-center gap-2 text-[10px] text-text-tertiary">
        <Badge tone={toneFor(snapshot.status, freshness)} testId={`${scope}-status`}>
          {t(`usageStatus.${snapshot.status}`)}
        </Badge>
        <span>{t(`usageFreshness.${freshness}`)}</span>
        {freshness === "stale" ? (
          <Badge tone="amber" testId={`${scope}-freshness`}>
            {t("usageStatus.stale")}
          </Badge>
        ) : null}
        {freshness === "stale" ? (
          <span className="text-state-warning-text">{t("usageStaleHint")}</span>
        ) : null}
        <span>·</span>
        <span>
          {t("usageCapturedAt")}: {localTime(snapshot.capturedAt)}
        </span>
        <span>·</span>
        <span>
          {t("usageSource")}: <code>{snapshot.source}</code>
        </span>
        <span>·</span>
        <span>{t(`usageSourceKind.${snapshot.sourceKind}`)}</span>
        {snapshot.attempts > 0 ? (
          <span>· {t("usageAttempts", { count: snapshot.attempts })}</span>
        ) : null}
      </div>
      {snapshot.error ? (
        <p className="mt-1 text-[10px] text-state-danger-text" data-testid={`${scope}-error`}>
          {snapshot.error.code}: {snapshot.error.message}
          {snapshot.nextAttemptAt
            ? ` · ${t("usageNextAttempt", { at: localTime(snapshot.nextAttemptAt) })}`
            : snapshot.retryAfterMs
              ? ` · ${t("usageRetryAfter", { seconds: Math.round(snapshot.retryAfterMs / 1000) })}`
              : ""}
        </p>
      ) : null}
      {snapshot.unavailableFields.length > 0 ? (
        <p className="mt-1 text-[10px] text-state-warning-text">
          {t("usageUnavailable")}: {snapshot.unavailableFields.join(", ")}
        </p>
      ) : null}
      {snapshot.note ? (
        <p className="mt-1 text-[10px] text-text-tertiary">{snapshot.note}</p>
      ) : null}
      {snapshot.samples.length > 0 ? (
        <table className="mt-2 w-full border-collapse text-left">
          <thead>
            <tr className="text-[10px] uppercase tracking-wide text-text-tertiary">
              <th className="py-1 pr-2 font-medium">{t("usageLabel")}</th>
              <th className="py-1 pr-2 font-medium">{t("usageValue")}</th>
              <th className="py-1 pr-2 font-medium">{t("usageWindow")}</th>
              <th className="py-1 pr-2 font-medium">{t("usageReset")}</th>
              <th className="py-1 font-medium">{t("usageProvenance")}</th>
            </tr>
          </thead>
          <tbody>
            {snapshot.samples.map((sample, index) => (
              <SampleRow key={`${sample.label}-${index}`} sample={sample} />
            ))}
          </tbody>
        </table>
      ) : (
        <p className="mt-1 text-[11px] text-text-tertiary" data-testid={`${scope}-no-numbers`}>
          {t("usageNoNumbers")}
        </p>
      )}
    </div>
  );
}

function PoolCard({ view, clock }: { view: PoolView; clock: number }) {
  const { t } = useTranslation("multicli");
  const scope = `usage-pool-${view.pool.id}`;
  const measured = view.snapshot.status === "ok" || view.snapshot.status === "partial";
  return (
    <li className="rounded-lg border border-separator-border px-4 py-3" data-testid={scope}>
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-sm font-medium text-text-primary">{view.pool.label}</span>
        <code className="text-[11px] text-text-tertiary">{view.pool.agentId}</code>
        <Badge tone={view.pool.identityStatus === "confirmed" ? "green" : "amber"}>
          {t(`usageIdentity.${view.pool.identityStatus}`)}
        </Badge>
        <span className="text-[10px] text-text-tertiary" data-testid={`${scope}-installations`}>
          {t("usagePoolInstallations", { count: view.pool.installationCount })}
        </span>
      </div>
      <SnapshotBody snapshot={view.snapshot} scope={scope} clock={clock} />
      {!measured && view.lastGood ? (
        <details className="mt-2 rounded-md border border-separator-border px-2 py-1">
          <summary className="cursor-pointer text-[10px] text-text-secondary">
            {t("usageLastGood", { at: localTime(view.lastGood.capturedAt) })}
          </summary>
          <SnapshotBody snapshot={view.lastGood} scope={`${scope}-last-good`} clock={clock} />
        </details>
      ) : null}
    </li>
  );
}

export default function UsageView({ api }: { api: MultiCliApi }) {
  const { t } = useTranslation("multicli");
  const [overview, setOverview] = useState<UsageOverviewView | null>(null);
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [error, setError] = useState<AppError | null>(null);
  const [refreshError, setRefreshError] = useState<AppError | null>(null);
  const [busy, setBusy] = useState(false);
  const [outcomes, setOutcomes] = useState<RefreshOutcome[]>([]);
  const [clock, setClock] = useState(() => Date.now());
  const mounted = useRef(true);
  /** Monotonic id of the newest request; an older answer must not overwrite it. */
  const requestSeq = useRef(0);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  // The freshness badge follows the wall clock while the page stays open. This
  // is a purely local recalculation: it never starts a CLI or a background read.
  useEffect(() => {
    const timer = window.setInterval(() => setClock(Date.now()), 15_000);
    return () => window.clearInterval(timer);
  }, []);

  const load = useCallback(async () => {
    const seq = ++requestSeq.current;
    setStatus("loading");
    try {
      const result = await api.usageOverview();
      if (!mounted.current || seq !== requestSeq.current) return;
      setOverview(result);
      setStatus("ready");
      setError(null);
    } catch (raw: unknown) {
      if (!mounted.current || seq !== requestSeq.current) return;
      setError(toAppError(raw));
      setStatus("error");
    } finally {
      if (mounted.current && seq === requestSeq.current) setBusy(false);
    }
  }, [api]);

  useEffect(() => {
    void load();
  }, [load]);

  const refresh = useCallback(
    async (force: boolean) => {
      const seq = ++requestSeq.current;
      setBusy(true);
      setRefreshError(null);
      try {
        // `force` is false for the normal entry point and for a retry, so the
        // backend's provider backoff is honoured. Only the explicitly labelled
        // force action may bypass it.
        const result = await api.usageRefresh(null, force);
        if (!mounted.current || seq !== requestSeq.current) return;
        setOverview(result.overview);
        setOutcomes(result.outcomes);
        setStatus("ready");
        setError(null);
      } catch (raw: unknown) {
        if (!mounted.current || seq !== requestSeq.current) return;
        // A failed refresh keeps the previous readings on screen and shows the
        // real error; it is never silent.
        setRefreshError(toAppError(raw));
      } finally {
        if (mounted.current && seq === requestSeq.current) setBusy(false);
      }
    },
    [api],
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col" data-testid="usage-view">
      <div className="flex flex-wrap items-center gap-2 border-b border-separator-border px-5 py-2">
        <h2 className="text-sm font-semibold">{t("usageTitle")}</h2>
        {overview ? (
          <span className="text-[11px] text-text-tertiary" data-testid="usage-generated-at">
            {t("usageGeneratedAt", { at: localTime(overview.generatedAt) })} · {timeZone()}
          </span>
        ) : null}
        {overview?.lastSuccessAt || overview?.lastAttemptAt ? (
          <span className="text-[10px] text-text-tertiary" data-testid="usage-last-times">
            {overview?.lastSuccessAt
              ? `${t("usageLastSuccess", { at: localTime(overview.lastSuccessAt) })}`
              : t("usageNeverSucceeded")}
            {overview?.lastAttemptAt
              ? ` · ${t("usageLastAttempt", { at: localTime(overview.lastAttemptAt) })}`
              : ""}
          </span>
        ) : null}
        <button
          type="button"
          className="ml-auto rounded-md border border-separator-border px-3 py-1 text-[11px] text-text-secondary hover:bg-background-primary-hover disabled:opacity-60"
          onClick={() => void refresh(false)}
          disabled={busy}
          data-testid="usage-refresh"
        >
          {busy ? t("usageRefreshing") : t("usageRefresh")}
        </button>
        <button
          type="button"
          className="rounded-md px-2 py-1 text-[10px] text-text-tertiary underline decoration-dotted underline-offset-2 hover:text-text-secondary disabled:opacity-60"
          onClick={() => void refresh(true)}
          disabled={busy}
          title={t("usageRefreshForceHint")}
          data-testid="usage-refresh-force"
        >
          {t("usageRefreshForce")}
        </button>
      </div>
      <p className="border-b border-separator-border px-5 py-2 text-[10px] leading-4 text-text-tertiary">
        {overview?.note ?? t("usageNote")}
      </p>

      <div className="min-h-0 flex-1 overflow-auto px-5 pb-8">
        {refreshError ? (
          <div className="mt-3">
            <ErrorPanel
              error={refreshError}
              onRetry={() => void refresh(false)}
              scope="usage-refresh"
            />
          </div>
        ) : null}
        {status === "loading" ? (
          <LoadingPanel label={t("loading")} testId="usage-loading" />
        ) : null}
        {status === "error" && error ? (
          <ErrorPanel error={error} onRetry={() => void load()} scope="usage" />
        ) : null}
        {status === "ready" && overview ? (
          <>
            <section className="mt-3">
              <h3 className="text-xs font-semibold text-text-primary">{t("usageAccounts")}</h3>
              {overview.accounts.length === 0 ? (
                <p className="mt-1 text-[11px] text-text-tertiary" data-testid="usage-accounts-empty">
                  {t("usageAccountsEmpty")}
                </p>
              ) : (
                <ul className="mt-2 flex flex-col gap-3">
                  {overview.accounts.map((entry) => (
                    <li key={entry.account.id}>
                      <div className="flex flex-wrap items-center gap-2 text-[11px] text-text-secondary">
                        <span className="font-medium text-text-primary">{entry.account.label}</span>
                        <span>· {entry.account.vendor}</span>
                        <span>· {entry.account.source}</span>
                        <span>
                          · {t("usageObservedAt")}: {localTime(entry.account.observedAt)}
                        </span>
                      </div>
                      <ul className="mt-2 flex flex-col gap-2">
                        {entry.pools.map((view) => (
                          <PoolCard key={view.pool.id} view={view} clock={clock} />
                        ))}
                      </ul>
                    </li>
                  ))}
                </ul>
              )}
            </section>

            <section className="mt-4">
              <h3 className="text-xs font-semibold text-text-primary">
                {t("usageUnconfirmedTitle")}
              </h3>
              <p className="mt-1 text-[10px] text-text-tertiary">{t("usageUnconfirmedHint")}</p>
              {overview.unconfirmedPools.length === 0 ? (
                <p className="mt-1 text-[11px] text-text-tertiary">{t("usageUnconfirmedEmpty")}</p>
              ) : (
                <ul className="mt-2 flex flex-col gap-2">
                  {overview.unconfirmedPools.map((view) => (
                    <PoolCard key={view.pool.id} view={view} clock={clock} />
                  ))}
                </ul>
              )}
            </section>

            {outcomes.length > 0 ? (
              <section className="mt-4" data-testid="usage-outcomes">
                <h3 className="text-xs font-semibold text-text-primary">{t("usageOutcomes")}</h3>
                <ul className="mt-1 flex flex-col gap-1 text-[10px] text-text-tertiary">
                  {outcomes.map((outcome) => (
                    <li key={outcome.agentId}>
                      <code>{outcome.agentId}</code> · {t(`usageStatus.${outcome.status}`)}
                      {outcome.skipped ? ` · ${t("usageSkipped")}` : ""}
                      {outcome.reason ? ` · ${outcome.reason}` : ""}
                    </li>
                  ))}
                </ul>
              </section>
            ) : null}

            <section className="mt-4">
              <h3 className="text-xs font-semibold text-text-primary">{t("usageCatalogTitle")}</h3>
              <p className="mt-1 text-[10px] text-text-tertiary">{t("usageCatalogHint")}</p>
              <div className="mt-2 overflow-x-auto">
                <table className="w-full border-collapse text-left text-[11px]">
                  <thead>
                    <tr className="text-[10px] uppercase tracking-wide text-text-tertiary">
                      <th className="py-1 pr-2 font-medium">{t("usageCatalogProduct")}</th>
                      <th className="py-1 pr-2 font-medium">{t("usageCatalogQuota")}</th>
                      <th className="py-1 pr-2 font-medium">{t("usageCatalogTokens")}</th>
                      <th className="py-1 pr-2 font-medium">{t("usageCatalogEntry")}</th>
                      <th className="py-1 pr-2 font-medium">{t("usageCatalogStatus")}</th>
                      <th className="py-1 font-medium">{t("usageCatalogLimitation")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {overview.catalog.map((entry) => (
                      <tr
                        key={entry.agentId}
                        className="border-t border-separator-border"
                        data-testid={`usage-catalog-${entry.agentId}`}
                      >
                        <td className="py-1 pr-2 text-text-primary">
                          {entry.product}
                          {entry.installed ? null : (
                            <span className="ml-1 text-[9px] text-text-tertiary">
                              ({t("usageNotInstalled")})
                            </span>
                          )}
                        </td>
                        <td className="py-1 pr-2 text-text-secondary">
                          {t(`usageAccess.${entry.quotaAccess}`)}
                        </td>
                        <td className="py-1 pr-2 text-text-secondary">
                          {t(`usageAccess.${entry.tokenAccess}`)}
                        </td>
                        <td className="py-1 pr-2 text-text-tertiary">
                          {entry.officialEntry ? <code>{entry.officialEntry}</code> : "—"}
                        </td>
                        <td className="py-1 pr-2">
                          <Badge
                            tone={
                              entry.status === "supported"
                                ? "green"
                                : entry.status === "unverified"
                                  ? "amber"
                                  : "neutral"
                            }
                          >
                            {t(`usageCatalogStatus.${entry.status}`)}
                          </Badge>
                        </td>
                        <td className="py-1 text-text-tertiary">{entry.limitation ?? "—"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </section>
          </>
        ) : null}
      </div>
    </div>
  );
}
