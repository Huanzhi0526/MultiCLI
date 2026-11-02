import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { MultiCliApi } from "../api";
import { Badge, EmptyPanel, ErrorPanel, LoadingPanel } from "../components";
import { toAppError } from "../navigation";
import type { PagedResource } from "../usePagedResource";
import type {
  AppError,
  HistoryCandidateView,
  HistoryOverallStatus,
  HistorySourceView,
  HistoryStatusView,
} from "../types";

const PRODUCTS = [
  { id: "codex", label: "Codex CLI" },
  { id: "claude-code", label: "Claude Code" },
  { id: "codebuddy-code", label: "CodeBuddy Code" },
];

export function statusTone(status: string): "neutral" | "blue" | "green" | "amber" | "red" {
  switch (status) {
    case "indexing":
      return "blue";
    case "ok":
      return "green";
    case "partial":
      return "amber";
    case "error":
      return "red";
    case "cancelled":
      return "amber";
    default:
      return "neutral";
  }
}

function StatusBadge({ status }: { status: HistoryStatusView }) {
  const { t } = useTranslation("multicli");
  return (
    <Badge tone={statusTone(status.status)} testId={`history-status-${status.status}`}>
      {t(`historyStatus${status.status.charAt(0).toUpperCase()}${status.status.slice(1)}`)}
    </Badge>
  );
}

function SourceStatusDetails({ status }: { status: HistoryStatusView }) {
  const { t } = useTranslation("multicli");
  return (
    <div className="mt-2 flex flex-col gap-1">
      <div
        className="h-1.5 w-full overflow-hidden rounded-full bg-background-secondary-default"
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.min(100, Math.round(status.progressPermille / 10))}
        aria-label={t("historyFiles", {
          done: status.filesDone,
          total: status.filesTotal,
          skipped: status.filesSkipped,
          failed: status.filesFailed,
        })}
      >
        <div
          className="h-full rounded-full bg-accent-500 transition-[width] duration-300"
          style={{ width: `${Math.min(100, status.progressPermille / 10)}%` }}
          data-testid="history-progress"
        />
      </div>
      <p className="flex flex-wrap gap-x-2 gap-y-1 text-[10px] text-text-tertiary">
        <span>{t("historyGeneration", { generation: status.generation })}</span>
        <span>
          {t("historyFiles", {
            done: status.filesDone,
            total: status.filesTotal,
            skipped: status.filesSkipped,
            failed: status.filesFailed,
          })}
        </span>
        <span>{t("historyMessagesIndexed", { count: status.messagesIndexed })}</span>
      </p>
      <p className="flex flex-wrap gap-x-2 text-[10px] text-text-tertiary">
        <span>{t("historyLastAttempt", { value: status.lastAttemptAt ?? t("historyNotAvailable") })}</span>
        <span>{t("historyLastSuccess", { value: status.lastSuccessAt ?? t("historyNotAvailable") })}</span>
      </p>
      {status.status === "ok" && !status.complete ? (
        <p className="text-[10px] text-amber-600" data-testid="history-incomplete">
          {t("historyStatusIncomplete")}
        </p>
      ) : null}
    </div>
  );
}

export default function SourcesPanel({
  api,
  sources,
  status,
  statusError,
  onChanged,
}: {
  api: MultiCliApi;
  sources: PagedResource<HistorySourceView>;
  status: HistoryOverallStatus | null;
  statusError: AppError | null;
  onChanged: () => void;
}) {
  const { t } = useTranslation("multicli");
  const [candidates, setCandidates] = useState<HistoryCandidateView[]>([]);
  const [candidatesError, setCandidatesError] = useState<AppError | null>(null);
  const [adding, setAdding] = useState(false);
  const [product, setProduct] = useState("codex");
  const [root, setRoot] = useState("");
  const [profile, setProfile] = useState("unconfirmed-local");
  const [busy, setBusy] = useState<string | null>(null);
  const [actionError, setActionError] = useState<AppError | null>(null);
  const [confirmingRebuild, setConfirmingRebuild] = useState<string | null>(null);

  const loadCandidates = useCallback(() => {
    setCandidatesError(null);
    void api
      .listHistoryCandidates()
      .then(setCandidates)
      .catch((raw: unknown) => setCandidatesError(toAppError(raw)));
  }, [api]);

  useEffect(() => {
    loadCandidates();
  }, [loadCandidates]);

  const statusById = useMemo(() => {
    const map = new Map<string, HistoryStatusView>();
    for (const item of status?.sources ?? []) {
      map.set(item.sourceId, item.status);
    }
    return map;
  }, [status]);

  const run = useCallback(
    async (key: string, action: () => Promise<unknown>) => {
      setBusy(key);
      setActionError(null);
      try {
        await action();
        onChanged();
      } catch (raw: unknown) {
        setActionError(toAppError(raw));
      } finally {
        setBusy(null);
      }
    },
    [onChanged],
  );

  const addSource = useCallback(() => {
    const trimmed = root.trim();
    if (!trimmed) {
      setActionError(toAppError({ code: "invalid_argument", message: t("historyRootRequired") }));
      return;
    }
    void run("add", () =>
      api.updateHistorySource({
        product,
        configProfileId: profile.trim() || "unconfirmed-local",
        root: trimmed,
        enabled: true,
      }),
    ).then(() => {
      setAdding(false);
      setRoot("");
      loadCandidates();
    });
  }, [api, loadCandidates, product, profile, root, run, t]);

  return (
    <aside className="flex w-[21rem] shrink-0 flex-col border-r border-separator-border">
      <div className="flex items-center gap-2 border-b border-separator-border px-4 py-3">
        <h2 className="text-sm font-semibold">{t("historySources")}</h2>
        <span className="text-[11px] text-text-tertiary" data-testid="history-source-count">
          {sources.items.length > 0
            ? t("listCount", { items: sources.items.length, total: sources.total })
            : ""}
        </span>
        {statusError ? (
          <Badge tone="amber" testId="history-status-unknown">
            {t("historyStatusUnknown")}
          </Badge>
        ) : null}
        <button
          type="button"
          className="ml-auto rounded-md border border-separator-border px-2 py-1 text-[11px] hover:bg-background-primary-hover disabled:opacity-50"
          onClick={() => {
            sources.refresh();
            onChanged();
            loadCandidates();
          }}
          data-testid="history-refresh-all"
        >
          {t("refresh")}
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-auto px-3 py-3">
        {sources.status === "loading" ? <LoadingPanel label={t("loading")} /> : null}
        {sources.status === "error" && sources.error ? (
          <ErrorPanel error={sources.error} onRetry={sources.refresh} scope="history-sources" compact />
        ) : null}
        {sources.status === "ready" && sources.items.length === 0 ? (
          <EmptyPanel
            title={t("historySourcesEmpty")}
            hint={t("historySourcesEmptyHint")}
            testId="history-sources-empty"
          />
        ) : null}

        <ul className="flex flex-col gap-2">
          {sources.items.map((source) => {
            const sourceStatus = statusById.get(source.sourceId) ?? source.status;
            const stale =
              !source.enabled ||
              sourceStatus.status === "error" ||
              sourceStatus.status === "partial";
            return (
              <li
                key={source.sourceId}
                className="rounded-lg border border-separator-border px-3 py-2"
                data-testid={`history-source-${source.sourceId}`}
              >
                <div className="flex flex-wrap items-center gap-1.5">
                  <span className="text-xs font-medium">{source.product}</span>
                  <Badge tone={source.enabled ? "green" : "neutral"}>
                    {source.enabled ? t("historyEnabled") : t("historyDisabled")}
                  </Badge>
                  <StatusBadge status={sourceStatus} />
                  {stale ? <Badge tone="amber">{t("historyStale")}</Badge> : null}
                </div>
                <p
                  className="mt-1 break-all text-[10px] text-text-tertiary"
                  title={source.root.displayPath}
                >
                  {source.root.displayPath}
                </p>
                <p className="mt-0.5 flex flex-wrap gap-2 text-[10px] text-text-tertiary">
                  <code>{source.agentId}</code>
                  <span>
                    {t("historyProfile")}: <code>{source.configProfileId}</code>
                  </span>
                  {source.configProfileStatus === "unconfirmed" ? (
                    <Badge tone="amber">{t("historyConfigProfileUnconfirmed")}</Badge>
                  ) : null}
                </p>

                <SourceStatusDetails status={sourceStatus} />
                {sourceStatus.error ? (
                  <div className="mt-2">
                    <ErrorPanel
                      error={sourceStatus.error}
                      onRetry={() => void run(source.sourceId, () => api.refreshHistory(source.sourceId))}
                      scope={`history-source-${source.sourceId}`}
                      compact
                    />
                  </div>
                ) : null}

                <div className="mt-2 flex flex-wrap gap-1.5">
                  <button
                    type="button"
                    className="rounded-md border border-separator-border px-2 py-1 text-[11px] hover:bg-background-primary-hover disabled:opacity-50"
                    disabled={busy === source.sourceId}
                    onClick={() =>
                      void run(source.sourceId, () =>
                        api.updateHistorySource({
                          sourceId: source.sourceId,
                          product: source.product,
                          installationId: source.installationId,
                          configProfileId: source.configProfileId,
                          root: source.root.displayPath,
                          enabled: !source.enabled,
                        }),
                      )
                    }
                    data-testid={`history-toggle-${source.sourceId}`}
                  >
                    {source.enabled ? t("historyDisable") : t("historyEnable")}
                  </button>
                  <button
                    type="button"
                    className="rounded-md border border-separator-border px-2 py-1 text-[11px] hover:bg-background-primary-hover disabled:opacity-50"
                    disabled={busy === source.sourceId}
                    onClick={() =>
                      void run(source.sourceId, () => api.refreshHistory(source.sourceId))
                    }
                    data-testid={`history-refresh-${source.sourceId}`}
                  >
                    {t("historyRefresh")}
                  </button>
                  {confirmingRebuild === source.sourceId ? (
                    <>
                      <button
                        type="button"
                        className="rounded-md border border-red-300 bg-red-50 px-2 py-1 text-[11px] text-red-700"
                        onClick={() => {
                          setConfirmingRebuild(null);
                          void run(source.sourceId, () =>
                            api.rebuildHistoryIndex(source.sourceId),
                          );
                        }}
                        data-testid={`history-rebuild-confirm-${source.sourceId}`}
                      >
                        {t("historyRebuildYes")}
                      </button>
                      <button
                        type="button"
                        className="rounded-md border border-separator-border px-2 py-1 text-[11px]"
                        onClick={() => setConfirmingRebuild(null)}
                      >
                        {t("historyRebuildNo")}
                      </button>
                    </>
                  ) : (
                    <button
                      type="button"
                      className="rounded-md border border-separator-border px-2 py-1 text-[11px] hover:bg-background-primary-hover disabled:opacity-50"
                      disabled={busy === source.sourceId}
                      onClick={() => setConfirmingRebuild(source.sourceId)}
                      title={t("historyRebuildConfirm")}
                      data-testid={`history-rebuild-${source.sourceId}`}
                    >
                      {t("historyRebuild")}
                    </button>
                  )}
                </div>
                {confirmingRebuild === source.sourceId ? (
                  <p className="mt-1 text-[10px] leading-4 text-text-tertiary">
                    {t("historyRebuildConfirm")}
                  </p>
                ) : null}
              </li>
            );
          })}
        </ul>

        {sources.status === "ready" && sources.cursor ? (
          <button
            type="button"
            className="mt-2 w-full rounded-md border border-separator-border px-3 py-1.5 text-xs text-text-secondary hover:bg-background-primary-hover"
            onClick={sources.loadMore}
            data-testid="history-sources-load-more"
          >
            {t("loadMore")}
          </button>
        ) : null}
        {sources.status === "loading-more" ? <LoadingPanel label={t("loading")} /> : null}
        {sources.status === "error-more" && sources.error ? (
          <ErrorPanel
            error={sources.error}
            onRetry={sources.loadMore}
            scope="history-sources-more"
            compact
          />
        ) : null}

        <div className="mt-4 border-t border-separator-border pt-3">
          <div className="flex items-center gap-2">
            <h3 className="text-xs font-semibold text-text-secondary">{t("historyAddSource")}</h3>
            <button
              type="button"
              className="ml-auto rounded-md border border-separator-border px-2 py-1 text-[11px]"
              onClick={() => setAdding((value) => !value)}
              data-testid="history-add-toggle"
            >
              {adding ? t("historyRebuildNo") : t("historyAddSource")}
            </button>
          </div>

          {adding ? (
            <div className="mt-2 flex flex-col gap-2">
              <label className="flex flex-col gap-1 text-[11px] text-text-tertiary">
                {t("historyProduct")}
                <select
                  className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 text-xs text-text-primary"
                  value={product}
                  onChange={(event) => setProduct(event.target.value)}
                  data-testid="history-add-product"
                >
                  {PRODUCTS.map((item) => (
                    <option key={item.id} value={item.id}>
                      {item.label}
                    </option>
                  ))}
                </select>
              </label>
              {candidatesError ? (
                <ErrorPanel
                  error={candidatesError}
                  onRetry={loadCandidates}
                  scope="history-candidates"
                  compact
                />
              ) : null}
              <label className="flex flex-col gap-1 text-[11px] text-text-tertiary">
                {t("historyCandidates")}
                <select
                  className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 text-xs text-text-primary"
                  value=""
                  onChange={(event) => {
                    const candidate = candidates.find(
                      (item) => item.root.displayPath === event.target.value,
                    );
                    if (candidate) {
                      setProduct(candidate.product);
                      setRoot(candidate.root.displayPath);
                    }
                  }}
                  data-testid="history-add-candidate"
                >
                  <option value="">—</option>
                  {candidates.map((candidate) => (
                    <option
                      key={`${candidate.product}|${candidate.root.displayPath}`}
                      value={candidate.root.displayPath}
                      disabled={!candidate.exists || !candidate.readable}
                    >
                      {candidate.displayName} · {candidate.root.displayPath}
                      {candidate.alreadyConfigured ? " ✓" : ""}
                      {!candidate.exists
                        ? ` · ${t("historyCandidateMissing")}`
                        : !candidate.readable
                          ? ` · ${t("historyCandidateUnreadable")}`
                          : ""}
                    </option>
                  ))}
                </select>
              </label>
              <label className="flex flex-col gap-1 text-[11px] text-text-tertiary">
                {t("historyRoot")}
                <input
                  className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 text-xs text-text-primary"
                  value={root}
                  onChange={(event) => setRoot(event.target.value)}
                  placeholder={t("historyManualRoot")}
                  data-testid="history-add-root"
                />
              </label>
              <label className="flex flex-col gap-1 text-[11px] text-text-tertiary">
                {t("historyProfile")}
                <input
                  className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 text-xs text-text-primary"
                  value={profile}
                  onChange={(event) => setProfile(event.target.value)}
                  data-testid="history-add-profile"
                />
              </label>
              <button
                type="button"
                className="rounded-md border border-accent-300 bg-accent-50 px-3 py-1.5 text-xs text-accent-700 disabled:opacity-50"
                disabled={busy === "add"}
                onClick={addSource}
                data-testid="history-add-submit"
              >
                {t("historyAddSource")}
              </button>
            </div>
          ) : null}

          {actionError ? (
            <div className="mt-2">
              <ErrorPanel error={actionError} scope="history-action" compact />
            </div>
          ) : null}
          {statusError ? (
            <div className="mt-2">
              <ErrorPanel error={statusError} onRetry={onChanged} scope="history-status" compact />
            </div>
          ) : null}
          <p className="mt-2 text-[10px] leading-4 text-text-tertiary">
            {candidates[0]?.note ?? t("historySourcesEmptyHint")}
          </p>
        </div>
      </div>
    </aside>
  );
}
