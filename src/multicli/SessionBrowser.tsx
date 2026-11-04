import { useCallback, useState } from "react";
import type { MultiCliApi } from "./api";
import { EntryTabs, ScopeList, SessionDetailView, SessionList } from "./components";
import { sessionFilterKey } from "./navigation";
import { usePagedResource, useSessionDetail } from "./usePagedResource";
import { ResumeBlock } from "./history/SessionPanel";
import type {
  AgentListItem,
  Capability,
  SessionOrigin,
  SessionSummary,
  WorkspaceListItem,
} from "./types";

export type Entry = "workspaces" | "agents";

/**
 * S01 navigation: one session opened from either the workspace or the agent
 * entry must resolve to the same session id, the same messages and the same
 * counts. This component is unchanged behaviourally; S02 only moved it out of
 * `App.tsx` so the discovery views can live beside it.
 */
export default function SessionBrowser({
  api,
  onResume,
  onNavigate,
}: {
  api: MultiCliApi;
  onResume?: (sessionId: string) => void;
  /** First-run guidance: an empty list points at the page that fills it. */
  onNavigate?: (view: "history" | "installations" | "terminal" | "runtime") => void;
}) {
  const [entry, setEntry] = useState<Entry>("workspaces");
  const [workspaceId, setWorkspaceId] = useState<string | null>(null);
  const [agentId, setAgentId] = useState<string | null>(null);
  const [origin, setOrigin] = useState<SessionOrigin | null>(null);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);

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
  // Metadata plus a bounded message page: the same session id and counts from
  // every entry, without ever loading a transcript of unknown length in one
  // response.
  const detail = useSessionDetail(api, selectedSessionId);
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

  return (
    <div className="flex min-h-0 flex-1" data-testid="session-browser">
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
          onOpenHistory={onNavigate ? () => onNavigate("history") : undefined}
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
          onOpenHistory={onNavigate ? () => onNavigate("history") : undefined}
        />
      </section>

      <main className="min-h-0 min-w-0 flex-1 overflow-auto">
        {detail.data?.session.nativeId ? <ResumeBlock api={api} sessionId={detail.data.session.id} onResume={onResume} /> : null}
        <SessionDetailView
          detail={detail}
          capabilities={capabilities}
          onRetry={refreshAll}
          onLoadMore={detail.loadMore}
        />
      </main>
    </div>
  );
}
