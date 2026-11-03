import { useCallback, useEffect, useRef, useState } from "react";
import type { MultiCliApi } from "../api";
import { usePagedResource } from "../usePagedResource";
import type { HistorySourceView } from "../types";
import SearchPanel from "./SearchPanel";
import SessionPanel from "./SessionPanel";
import SourcesPanel from "./SourcesPanel";
import { useHistorySession, useHistoryStatus, type SessionTarget } from "./useHistory";

export default function HistoryView({ api, onResume }: { api: MultiCliApi; onResume?: (sessionId: string) => void }) {
  const sources = usePagedResource<HistorySourceView>(
    "history-sources",
    (cursor) => api.listHistorySources({ limit: 50, cursor }),
    (item) => item.sourceId,
  );
  const { status, error: statusError } = useHistoryStatus(api, true);
  const [target, setTarget] = useState<SessionTarget | null>(null);
  const session = useHistorySession(api, target);
  const seenGenerations = useRef(new Map<string, number>());

  const refreshSources = useCallback(() => {
    sources.refresh();
  }, [sources]);

  // A completed pass changes the number of indexed messages; refresh the config
  // rows once per new generation instead of on every status poll.
  useEffect(() => {
    if (!status) {
      return;
    }
    let changed = false;
    for (const item of status.sources) {
      const seen = seenGenerations.current.get(item.sourceId);
      if (seen !== item.status.generation) {
        if (seen !== undefined && item.status.generation > seen) {
          changed = true;
        }
        seenGenerations.current.set(item.sourceId, item.status.generation);
      }
    }
    if (changed && !status.inFlight) {
      sources.refresh();
    }
  }, [status, sources]);

  const open = useCallback((sessionId: string, messageId?: string | null) => {
    setTarget((current) => ({
      sessionId,
      anchorMessageId: messageId ?? null,
      nonce: (current?.nonce ?? 0) + 1,
    }));
  }, []);

  return (
    <div className="flex min-h-0 flex-1" data-testid="history-view">
      <SourcesPanel
        api={api}
        sources={sources}
        status={status}
        statusError={statusError}
        onChanged={refreshSources}
      />
      <SearchPanel api={api} sources={sources.items} onOpen={open} />
      <main className="min-h-0 min-w-0 flex-1 overflow-auto">
        <SessionPanel api={api} state={session} onResume={onResume} />
      </main>
    </div>
  );
}
