import { localTime, TechnicalDetails } from "../presentation";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Badge, ErrorPanel } from "../components";
import type { AppError, DiscoverySettings, ScanStatus, ScanView } from "../types";

export const INTERVAL_OPTIONS = [3000, 5000, 10000, 30000];

/**
 * The product families declared by the signature registry: three core (S02/S05),
 * six extensions and one observed-only family (S06). A family with no positive
 * sample is not a claim, so this list mirrors the registry instead of being
 * hand-maintained per stage.
 */
export const DISCOVERY_AGENTS = [
  "codex",
  "claude",
  "codebuddy",
  "gemini",
  "opencode",
  "qwen",
  "copilot",
  "kimi",
  "aider",
  "trae",
] as const;

const scanTone: Record<ScanStatus, "neutral" | "blue" | "green" | "amber" | "red"> = {
  idle: "neutral",
  scanning: "blue",
  ok: "green",
  partial: "amber",
  error: "red",
};

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="text-[10px] uppercase tracking-wide text-text-tertiary">{label}</dt>
      <dd className="min-w-0 text-xs text-text-primary [overflow-wrap:anywhere]">{children}</dd>
    </div>
  );
}

/**
 * Read-only scan summary. Every value comes from the backend `scan` object:
 * the status, the generation, the two timestamps, the counts and the explicit
 * note that short-lived processes can be missed.
 */
export function ScanStatusBar({
  scan,
  busy,
  error,
  settings,
  settingsError,
  onRefresh,
  onIntervalChange,
  mode = "processes",
}: {
  mode?: "installations" | "processes";
  scan: ScanView | null;
  busy: boolean;
  error: AppError | null;
  settings: DiscoverySettings | null;
  settingsError: AppError | null;
  onRefresh: () => void;
  onIntervalChange: (intervalMs: number) => void;
}) {
  const { t } = useTranslation("multicli");
  return (
    <section
      className="flex flex-col gap-2 border-b border-separator-border bg-background-secondary-default/40 px-5 py-3"
      data-testid="scan-status"
    >
      <div className="flex flex-wrap items-center gap-2">
        <Badge tone={scan ? scanTone[scan.status] : "neutral"} testId="scan-status-badge">
          {t(`scanStatus.${scan?.status ?? "idle"}`)}
        </Badge>
        <span className="text-[11px] text-text-tertiary" data-testid="scan-generation">
          {t("scanGeneration", { generation: scan?.generation ?? 0 })}
        </span>
        <span className="text-[11px] text-text-tertiary">
          {t("scanLastAttempt", { value: localTime(scan?.lastAttemptAt) })}
        </span>
        <span className="text-[11px] text-text-tertiary" data-testid="scan-last-success">
          {t("scanLastSuccess", { value: localTime(scan?.lastSuccessAt) })}
        </span>
        {scan && mode === "processes" ? (
          <span className="text-[11px] text-text-tertiary">
            {t("scanCounts", {
              sampled: scan.sampledProcesses,
              tracked: scan.trackedProcesses,
              unreadable: scan.unreadableProcesses,
            })}
          </span>
        ) : null}
        {mode === "processes" ? <label className="ml-auto flex items-center gap-1 text-[11px] text-text-tertiary">
          {t("scanInterval")}
          <select
            className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 text-[11px] text-text-primary"
            value={settings?.intervalMs ?? scan?.intervalMs ?? 3000}
            onChange={(event) => onIntervalChange(Number(event.target.value))}
            data-testid="scan-interval"
          >
            {INTERVAL_OPTIONS.map((value) => (
              <option key={value} value={value}>
                {t("scanIntervalValue", { seconds: value / 1000 })}
              </option>
            ))}
          </select>
        </label> : null}
        <button
          type="button"
          className="rounded-md border border-separator-border px-3 py-1 text-[11px] text-text-secondary hover:bg-background-primary-hover disabled:opacity-50"
          onClick={onRefresh}
          disabled={busy}
          data-testid="scan-refresh"
        >
          {busy ? t("scanRunning") : t("scanRefresh")}
        </button>
      </div>
      {mode === "processes" ? <p
        className="text-[10px] leading-4 text-text-tertiary"
        data-testid="scan-note"
        title={scan?.note ?? undefined}
      >
        {t("scanNoteFallback")}
      </p> : null}
      {settingsError ? (
        <ErrorPanel error={settingsError} scope="scan-settings" compact />
      ) : null}
      {scan?.installError ? (
        <ErrorPanel error={scan.installError} scope="scan-install" compact />
      ) : null}
      {error ? (
        <ErrorPanel error={error} scope="scan" compact onRetry={onRefresh} />
      ) : scan?.error ? (
        <ErrorPanel error={scan.error} scope="scan" compact onRetry={onRefresh} />
      ) : null}
    </section>
  );
}

export function ReadOnlyNote() {
  const { t } = useTranslation("multicli");
  return (
    <TechnicalDetails label={t("help")}><p className="text-xs leading-5 text-text-tertiary" data-testid="readonly-note">
      {t("discoveryReadOnlyNote")}
    </p></TechnicalDetails>
  );
}
