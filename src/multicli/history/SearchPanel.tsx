import { localTime, pathName, TechnicalDetails } from "../presentation";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { MultiCliApi } from "../api";
import { Badge, EmptyPanel, ErrorPanel, LoadingPanel } from "../components";
import { usePagedResource } from "../usePagedResource";
import type {
  HistorySourceView,
  HistorySearchQuery,
  SessionSummary,
  WorkspaceListItem,
} from "../types";
import { useHistorySearch } from "./useHistory";

export default function SearchPanel({
  api,
  sources,
  onOpen,
}: {
  api: MultiCliApi;
  sources: HistorySourceView[];
  onOpen: (sessionId: string, messageId?: string | null) => void;
}) {
  const { t } = useTranslation("multicli");
  const [text, setText] = useState("");
  const [debounced, setDebounced] = useState("");
  const [agentId, setAgentId] = useState("");
  const [sourceId, setSourceId] = useState("");
  const [workspaceId, setWorkspaceId] = useState("");

  useEffect(() => {
    const timer = window.setTimeout(() => setDebounced(text.trim()), 300);
    return () => window.clearTimeout(timer);
  }, [text]);

  const agents = useMemo(() => {
    const seen = new Map<string, string>();
    for (const source of sources) {
      seen.set(source.agentId, source.product);
    }
    return [...seen.entries()];
  }, [sources]);

  const workspaces = usePagedResource<WorkspaceListItem>(
    "history-search-workspaces",
    (cursor) => api.listWorkspaces({ limit: 100, cursor }),
    (item) => item.workspace.id,
  );

  const query: HistorySearchQuery | null = debounced
    ? {
        text: debounced,
        ...(agentId ? { agentId } : {}),
        ...(sourceId ? { sourceId } : {}),
        ...(workspaceId ? { workspaceId } : {}),
        limit: 20,
      }
    : null;
  const search = useHistorySearch(api, query);

  const browse = usePagedResource<SessionSummary>(
    "history-browse-sessions",
    (cursor) => api.listSessions({ limit: 20, cursor, origin: "history-only" }),
    (item) => item.session.id,
  );

  const clearFilters = useCallback(() => {
    setAgentId("");
    setSourceId("");
    setWorkspaceId("");
  }, []);

  const hasFilters = Boolean(agentId || sourceId || workspaceId);

  return (
    <section className="flex w-[27rem] shrink-0 flex-col border-r border-separator-border">
      <div className="flex items-center gap-2 border-b border-separator-border px-4 py-3">
        <h2 className="text-sm font-semibold">{t("historySearch")}</h2>
        {debounced && search.status !== "loading" && search.status !== "error" ? (
          <span className="text-[11px] text-text-tertiary" data-testid="history-search-total">
            {t("historySearchTotal", { total: search.total })}
          </span>
        ) : null}
      </div>

      <div className="flex flex-col gap-2 border-b border-separator-border px-4 py-2">
        <input
          className="rounded-md border border-separator-border bg-background-primary-default px-3 py-1.5 text-xs text-text-primary"
          value={text}
          onChange={(event) => setText(event.target.value)}
          placeholder={t("historySearchPlaceholder")}
          aria-label={t("historySearch")}
          data-testid="history-search-input"
        />
        <div className="flex flex-wrap items-center gap-1.5 text-[11px]">
          <select
            className="rounded-md border border-separator-border bg-background-primary-default px-1.5 py-1 text-[11px]"
            value={agentId}
            onChange={(event) => setAgentId(event.target.value)}
            aria-label={t("historyFilterAgent")}
            data-testid="history-filter-agent"
          >
            <option value="">{t("historyFilterAgent")}: {t("historyFilterAll")}</option>
            {agents.map(([id, product]) => (
              <option key={id} value={id}>
                {product} ({id})
              </option>
            ))}
          </select>
          <select
            className="max-w-[10rem] rounded-md border border-separator-border bg-background-primary-default px-1.5 py-1 text-[11px]"
            value={sourceId}
            onChange={(event) => setSourceId(event.target.value)}
            aria-label={t("historyFilterSource")}
            data-testid="history-filter-source"
          >
            <option value="">{t("historyFilterSource")}: {t("historyFilterAll")}</option>
            {sources.map((source) => (
              <option key={source.sourceId} value={source.sourceId}>
                {source.product} · {source.root.displayPath}
              </option>
            ))}
          </select>
          <select
            className="max-w-[11rem] rounded-md border border-separator-border bg-background-primary-default px-1.5 py-1 text-[11px]"
            value={workspaceId}
            onChange={(event) => setWorkspaceId(event.target.value)}
            aria-label={t("historyFilterWorkspace")}
            data-testid="history-filter-workspace"
          >
            <option value="">{t("historyFilterWorkspace")}: {t("historyFilterAll")}</option>
            {workspaces.items.map((item) => (
              <option key={item.workspace.id} value={item.workspace.id}>
                {pathName(item.workspace.path.displayPath)}
              </option>
            ))}
          </select>
          {hasFilters ? (
            <button
              type="button"
              className="rounded-md border border-separator-border px-1.5 py-1 text-[11px]"
              onClick={clearFilters}
            >
              {t("historyFilterAll")}
            </button>
          ) : null}
        </div>
        <TechnicalDetails label={t("help")}><p>{t("historySearchHint")}</p></TechnicalDetails>
      </div>

      <div className="min-h-0 flex-1 overflow-auto px-3 py-3">
        {!debounced ? (
          <>
            <div className="mb-2 flex items-center gap-2">
              <h3 className="text-xs font-semibold text-text-secondary">{t("historyBrowse")}</h3>
              <button
                type="button"
                className="ml-auto rounded-md border border-separator-border px-2 py-0.5 text-[11px]"
                onClick={browse.refresh}
                data-testid="history-browse-refresh"
              >
                {t("refresh")}
              </button>
            </div>
            {browse.status === "loading" ? <LoadingPanel label={t("loading")} /> : null}
            {browse.status === "error" && browse.error ? (
              <ErrorPanel error={browse.error} onRetry={browse.refresh} scope="history-browse" compact />
            ) : null}
            {browse.status === "ready" && browse.items.length === 0 ? (
              <EmptyPanel
                title={t("historyBrowseEmpty")}
                hint={t("historyBrowseEmptyHint")}
                testId="history-browse-empty"
              />
            ) : null}
            {sources.length === 0 && browse.items.length > 0 ? <p className="mb-3 text-xs leading-5 text-text-tertiary">{t("historyCachedHint")}</p> : null}
            <ul className="flex flex-col gap-1">
              {browse.items.map((item) => (
                <li key={item.session.id}>
                  <button
                    type="button"
                    className="w-full rounded-lg border border-transparent px-3 py-2 text-left hover:bg-background-primary-hover"
                    onClick={() => onOpen(item.session.id, null)}
                    data-testid={`history-session-${item.session.id}`}
                  >
                    <span className="block truncate text-xs font-medium text-text-primary">
                      {item.session.title || t("untitledSession")}
                    </span>
                    <span className="mt-1 flex flex-wrap items-center gap-1.5 text-[11px] text-text-tertiary">
                      <Badge tone="neutral">{t("historyReadOnly")}</Badge>
                      <code>{item.session.agentId}</code>
                      <span>{t("messageCount", { count: item.messageCount })}</span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
            {browse.status === "ready" && browse.cursor ? (
              <button
                type="button"
                className="mt-2 w-full rounded-md border border-separator-border px-3 py-1.5 text-xs"
                onClick={browse.loadMore}
                data-testid="history-browse-load-more"
              >
                {t("loadMore")}
              </button>
            ) : null}
            {browse.status === "loading-more" ? <LoadingPanel label={t("loading")} /> : null}
            {browse.status === "error-more" && browse.error ? (
              <ErrorPanel
                error={browse.error}
                onRetry={browse.loadMore}
                scope="history-browse-more"
                compact
              />
            ) : null}
          </>
        ) : (
          <>
            <div className="mb-2 flex flex-wrap items-center gap-2 text-[10px] text-text-tertiary">
              <Badge tone={search.matchMode === "fts" ? "blue" : "amber"}>
                {t(search.matchMode === "fts" ? "historySearchNoteFts" : "historySearchNoteLike")}
              </Badge>
              <span>
                {search.matchMode === "fts"
                  ? t("historySearchNoteFts")
                  : t("historySearchNoteLike")}
              </span>
            </div>
            {search.status === "loading" ? (
              <LoadingPanel label={t("loading")} testId="history-search-loading" />
            ) : null}
            {search.status === "error" && search.error ? (
              <ErrorPanel
                error={search.error}
                onRetry={search.retry}
                scope="history-search"
              />
            ) : null}
            {search.status === "ready" && search.hits.length === 0 ? (
              <EmptyPanel
                title={t("historySearchNoResults")}
                hint={t("historySearchHint")}
                testId="history-search-empty"
              />
            ) : null}
            <ul className="flex flex-col gap-1">
              {search.hits.map((hit) => (
                <li key={hit.messageId}>
                  <button
                    type="button"
                    className="w-full rounded-lg border border-transparent px-3 py-2 text-left hover:bg-background-primary-hover"
                    onClick={() => onOpen(hit.sessionId, hit.messageId)}
                    data-testid={`history-hit-${hit.messageId}`}
                  >
                    <span className="block truncate text-xs font-medium text-text-primary">
                      {hit.sessionTitle || t("untitledSession")}
                    </span>
                    <span className="mt-1 flex flex-wrap items-center gap-1.5 text-[10px] text-text-tertiary">
                      <code>{hit.agentId}</code>
                      <span>{t(`value.${hit.kind}`, { defaultValue: hit.kind })}</span>
                      <span>{localTime(hit.observedAt)}</span>
                      <span>{t("messageCount", { count: hit.sessionMessageCount })}</span>
                    </span>
                    <span className="mt-1 block whitespace-pre-wrap text-[11px] leading-4 text-text-secondary">
                      {hit.snippet}
                      {hit.snippetTruncated ? ` ${t("historySnippetTruncated")}` : ""}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
            {search.status === "ready" && search.hasMore ? (
              <button
                type="button"
                className="mt-2 w-full rounded-md border border-separator-border px-3 py-1.5 text-xs"
                onClick={search.loadMore}
                data-testid="history-search-load-more"
              >
                {t("loadMore")}
              </button>
            ) : null}
            {search.status === "loading-more" ? <LoadingPanel label={t("loading")} /> : null}
            {search.status === "error-more" && search.error ? (
              <ErrorPanel
                error={search.error}
                onRetry={search.loadMore}
                scope="history-search-more"
                compact
              />
            ) : null}
          </>
        )}
      </div>
    </section>
  );
}
