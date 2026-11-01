import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import "@/i18n/multicli";
import { multiCliApi } from "./api";
import type { MultiCliApi } from "./api";
import {
  EntryTabs,
  ErrorPanel,
  ScopeList,
  SessionDetailView,
  SessionList,
} from "./components";
import { sessionFilterKey, toAppError } from "./navigation";
import { useDetailResource, usePagedResource } from "./usePagedResource";
import type {
  AgentListItem,
  AppError,
  Capability,
  DiagnosticsExport,
  SessionDetail,
  SessionOrigin,
  SessionSummary,
  WorkspaceListItem,
} from "./types";

type Entry = "workspaces" | "agents";

interface ExportState {
  status: "idle" | "pending" | "done" | "error";
  result?: DiagnosticsExport;
  error?: AppError;
}

export default function App({ api = multiCliApi }: { api?: MultiCliApi }) {
  const { t } = useTranslation("multicli");
  const [entry, setEntry] = useState<Entry>("workspaces");
  const [workspaceId, setWorkspaceId] = useState<string | null>(null);
  const [agentId, setAgentId] = useState<string | null>(null);
  const [origin, setOrigin] = useState<SessionOrigin | null>(null);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [exportState, setExportState] = useState<ExportState>({ status: "idle" });

  useEffect(() => {
    // Keep the inherited boot watchdog from replacing a healthy mounted page.
    document.documentElement.dataset.appMounted = "1";
  }, []);

  const workspaces = usePagedResource<WorkspaceListItem>(
    "workspaces",
    (cursor) => api.listWorkspaces({ limit: 100, cursor }),
    (item) => item.workspace.id,
  );
  const agents = usePagedResource<AgentListItem>(
    "agents",
    (cursor) => api.listAgents({ limit: 100, cursor }),
    (item) => item.agent.id,
  );
  const activeWorkspaceId = entry === "workspaces" ? workspaceId : null;
  const activeAgentId = entry === "agents" ? agentId : null;
  const sessionsKey = sessionFilterKey(entry, activeWorkspaceId, activeAgentId, origin);
  const sessions = usePagedResource<SessionSummary>(
    sessionsKey,
    (cursor) =>
      api.listSessions({
        limit: 20,
        cursor,
        workspaceId: activeWorkspaceId,
        agentId: activeAgentId,
        origin,
      }),
    (item) => item.session.id,
  );
  const detail = useDetailResource<SessionDetail>(selectedSessionId, (id) =>
    api.getSession(id),
  );
  const detailAgentId = detail.data?.session.agentId ?? null;
  const capabilities = usePagedResource<Capability>(
    detailAgentId ? `capabilities|${detailAgentId}` : null,
    (cursor) =>
      detailAgentId
        ? api.listCapabilities({ agentId: detailAgentId, limit: 50, cursor })
        : Promise.resolve({ items: [], nextCursor: null, total: 0 }),
    (item) => `${item.name}|${item.installationId ?? ""}`,
  );

  const clearSelection = useCallback(() => {
    setSelectedSessionId(null);
  }, []);

  const selectEntry = useCallback(
    (next: Entry) => {
      setEntry(next);
      clearSelection();
    },
    [clearSelection],
  );

  const selectAll = useCallback(() => {
    if (entry === "workspaces") {
      setWorkspaceId(null);
    } else {
      setAgentId(null);
    }
    clearSelection();
  }, [clearSelection, entry]);

  const selectWorkspace = useCallback(
    (id: string) => {
      setEntry("workspaces");
      setWorkspaceId(id);
      clearSelection();
    },
    [clearSelection],
  );

  const selectAgent = useCallback(
    (id: string) => {
      setEntry("agents");
      setAgentId(id);
      clearSelection();
    },
    [clearSelection],
  );

  const selectSession = useCallback((id: string) => {
    setSelectedSessionId(id);
  }, []);

  const changeOrigin = useCallback(
    (next: SessionOrigin | null) => {
      setOrigin(next);
      clearSelection();
    },
    [clearSelection],
  );

  const refreshAll = useCallback(() => {
    workspaces.refresh();
    agents.refresh();
    sessions.refresh();
    if (selectedSessionId) {
      detail.refresh();
      capabilities.refresh();
    }
  }, [agents, capabilities, detail, selectedSessionId, sessions, workspaces]);

  const exportDiagnostics = useCallback(async () => {
    setExportState({ status: "pending" });
    try {
      const result = await api.exportDiagnostics();
      setExportState({ status: "done", result });
    } catch (raw: unknown) {
      setExportState({ status: "error", error: toAppError(raw) });
    }
  }, [api]);

  return (
    <div
      className="flex h-full min-h-0 flex-col bg-background-primary-default text-text-primary"
      data-testid="app-root"
    >
      <header className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-separator-border px-5 py-3">
        <div>
          <h1 className="text-base font-semibold">MultiCLI</h1>
          <p className="text-[11px] text-text-tertiary">{t("stage")}</p>
        </div>
        <div className="ml-auto flex flex-wrap items-center gap-2">
          {exportState.status === "done" && exportState.result ? (
            <span
              className="max-w-[28rem] truncate text-[11px] text-state-success-text"
              data-testid="export-result"
              title={exportState.result.path}
            >
              {t("exportSuccess", {
                path: exportState.result.path,
                createdAt: exportState.result.createdAt,
              })}
            </span>
          ) : null}
          <button
            type="button"
            className="rounded-md border border-separator-border bg-background-primary-default px-3 py-1.5 text-xs hover:bg-background-primary-hover disabled:opacity-60"
            disabled={exportState.status === "pending"}
            onClick={() => void exportDiagnostics()}
            data-testid="export-button"
          >
            {exportState.status === "pending" ? t("exporting") : t("exportDiagnostics")}
          </button>
        </div>
        <p className="w-full text-[10px] text-text-tertiary">{t("diagnosticsHint")}</p>
        {exportState.status === "error" && exportState.error ? (
          <div className="w-full">
            <ErrorPanel
              error={exportState.error}
              onRetry={() => void exportDiagnostics()}
              scope="export"
              compact
            />
          </div>
        ) : null}
      </header>

      <div className="flex min-h-0 flex-1">
        <aside className="flex w-72 shrink-0 flex-col border-r border-separator-border">
          <div className="p-3">
            <EntryTabs entry={entry} onChange={selectEntry} />
          </div>
          <ScopeList
            entry={entry}
            workspaces={workspaces}
            agents={agents}
            selectedId={entry === "workspaces" ? workspaceId : agentId}
            onSelectAll={selectAll}
            onSelectWorkspace={selectWorkspace}
            onSelectAgent={selectAgent}
            onRetry={refreshAll}
          />
        </aside>

        <section className="flex w-[24rem] shrink-0 flex-col border-r border-separator-border">
          <SessionList
            sessions={sessions}
            selectedId={selectedSessionId}
            origin={origin}
            onOriginChange={changeOrigin}
            onSelect={selectSession}
            onRefresh={refreshAll}
            onRetry={refreshAll}
          />
        </section>

        <main className="min-h-0 min-w-0 flex-1 overflow-auto">
          <SessionDetailView
            detail={detail}
            capabilities={capabilities}
            onRetry={refreshAll}
          />
        </main>
      </div>
    </div>
  );
}
