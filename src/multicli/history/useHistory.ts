/**
 * History view state: status polling, bounded message paging and search.
 *
 * The same rules as S01/S02 apply: every request carries a token so a late
 * response cannot overwrite a newer selection, pages merge by id without
 * reordering, and a failure is surfaced as a real AppError instead of an empty
 * list. Search and browse open the same session and the same message ids.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import type { MultiCliApi } from "../api";
import { mergePageItems, RequestGate, toAppError } from "../navigation";
import type {
  AppError,
  HistoryOverallStatus,
  HistorySearchQuery,
  Message,
  MessagePage,
  SearchHitView,
  SessionMetadataView,
} from "../types";

export interface HistoryStatusState {
  status: HistoryOverallStatus | null;
  error: AppError | null;
}

/** Polls the indexer while the history page is mounted. */
export function useHistoryStatus(api: MultiCliApi, active: boolean): HistoryStatusState {
  const [state, setState] = useState<HistoryStatusState>({ status: null, error: null });
  const gateRef = useRef<RequestGate | null>(null);
  if (!gateRef.current) {
    gateRef.current = new RequestGate();
  }

  useEffect(() => {
    if (!active) {
      return;
    }
    const gate = gateRef.current as RequestGate;
    let stopped = false;
    const tick = () => {
      const token = gate.begin();
      void api
        .getHistoryStatus()
        .then((status) => {
          if (!gate.isCurrent(token) || stopped) {
            return;
          }
          setState({ status, error: null });
        })
        .catch((raw: unknown) => {
          if (!gate.isCurrent(token) || stopped) {
            return;
          }
          setState((current) => ({ ...current, error: toAppError(raw) }));
        });
    };
    tick();
    const timer = window.setInterval(tick, 1500);
    return () => {
      stopped = true;
      window.clearInterval(timer);
      gate.invalidate();
    };
  }, [active, api]);

  return state;
}

export interface SessionTarget {
  sessionId: string;
  anchorMessageId?: string | null;
  /** Bumped on every open so re-opening the same hit refetches. */
  nonce: number;
}

export type SessionLoadStatus =
  | "idle"
  | "loading"
  | "ready"
  | "error"
  | "loading-more"
  | "error-more";

export interface HistorySessionState {
  metadata: SessionMetadataView | null;
  page: MessagePage | null;
  status: SessionLoadStatus;
  error: AppError | null;
  loadMore: () => void;
  retry: () => void;
}

/**
 * Loads metadata and the first bounded message page for one session. With an
 * anchor the page starts a quarter-page before the hit and reports the anchor
 * index so the UI can scroll to the exact row.
 */
export function useHistorySession(
  api: MultiCliApi,
  target: SessionTarget | null,
): HistorySessionState {
  const [metadata, setMetadata] = useState<SessionMetadataView | null>(null);
  const [page, setPage] = useState<MessagePage | null>(null);
  const [status, setStatus] = useState<SessionLoadStatus>("idle");
  const [error, setError] = useState<AppError | null>(null);
  const [epoch, setEpoch] = useState(0);
  const gateRef = useRef<RequestGate | null>(null);
  if (!gateRef.current) {
    gateRef.current = new RequestGate();
  }

  useEffect(() => {
    const gate = gateRef.current as RequestGate;
    if (!target) {
      gate.invalidate();
      setMetadata(null);
      setPage(null);
      setStatus("idle");
      setError(null);
      return;
    }
    const token = gate.begin();
    setStatus("loading");
    setError(null);
    setPage(null);
    setMetadata(null);
    void Promise.all([
      api.getSessionMetadata(target.sessionId),
      api.listSessionMessages({
        sessionId: target.sessionId,
        limit: 50,
        anchorMessageId: target.anchorMessageId ?? null,
      }),
    ])
      .then(([nextMetadata, nextPage]) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setMetadata(nextMetadata);
        setPage(nextPage);
        setStatus("ready");
      })
      .catch((raw: unknown) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setMetadata(null);
        setPage(null);
        setStatus("error");
        setError(toAppError(raw));
      });
    return () => {
      gate.invalidate();
    };
  }, [api, target?.sessionId, target?.anchorMessageId, target?.nonce, epoch]);

  const loadMore = useCallback(() => {
    if (!target || !page?.nextCursor || status === "loading-more") {
      return;
    }
    const gate = gateRef.current as RequestGate;
    const token = gate.begin();
    setStatus("loading-more");
    setError(null);
    void api
      .listSessionMessages({
        sessionId: target.sessionId,
        limit: 50,
        cursor: page.nextCursor,
      })
      .then((next) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setPage((current) =>
          current
            ? {
                ...next,
                items: mergePageItems<Message>(current.items, next.items, (item) => item.id),
                anchorMessageId: current.anchorMessageId,
                anchorIndex: current.anchorIndex,
              }
            : next,
        );
        setStatus("ready");
      })
      .catch((raw: unknown) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setStatus("error-more");
        setError(toAppError(raw));
      });
  }, [api, page?.nextCursor, status, target]);

  const retry = useCallback(() => {
    setEpoch((value) => value + 1);
  }, []);

  return { metadata, page, status, error, loadMore, retry };
}

export interface HistorySearchState {
  hits: SearchHitView[];
  total: number;
  matchMode: "fts" | "like";
  note: string;
  hasMore: boolean;
  status: SessionLoadStatus;
  error: AppError | null;
  loadMore: () => void;
  retry: () => void;
}

/** Debounced search. `query` must already be normalized by the caller. */
export function useHistorySearch(
  api: MultiCliApi,
  query: HistorySearchQuery | null,
): HistorySearchState {
  const [hits, setHits] = useState<SearchHitView[]>([]);
  const [total, setTotal] = useState(0);
  const [matchMode, setMatchMode] = useState<"fts" | "like">("fts");
  const [note, setNote] = useState("");
  const [status, setStatus] = useState<SessionLoadStatus>("idle");
  const [error, setError] = useState<AppError | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [epoch, setEpoch] = useState(0);
  const gateRef = useRef<RequestGate | null>(null);
  if (!gateRef.current) {
    gateRef.current = new RequestGate();
  }
  const key = query
    ? JSON.stringify([query.text, query.workspaceId, query.agentId, query.sourceId])
    : null;

  useEffect(() => {
    const gate = gateRef.current as RequestGate;
    if (!query) {
      gate.invalidate();
      setHits([]);
      setTotal(0);
      setCursor(null);
      setStatus("idle");
      setError(null);
      return;
    }
    const token = gate.begin();
    setStatus("loading");
    setError(null);
    // A new key must not show the previous keyword's rows under the new one.
    setHits([]);
    setTotal(0);
    void api
      .searchHistory(query)
      .then((page) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setHits(page.items);
        setTotal(page.total);
        setMatchMode(page.matchMode);
        setNote(page.note);
        setCursor(page.nextCursor);
        setStatus("ready");
      })
      .catch((raw: unknown) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setHits([]);
        setTotal(0);
        setStatus("error");
        setError(toAppError(raw));
      });
    return () => {
      gate.invalidate();
    };
    // `query` is represented by `key`; the object identity changes on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api, key, epoch]);

  const loadMore = useCallback(() => {
    if (!query || !cursor || status === "loading-more") {
      return;
    }
    const gate = gateRef.current as RequestGate;
    const token = gate.begin();
    setStatus("loading-more");
    void api
      .searchHistory({ ...query, cursor })
      .then((page) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setHits((current) => mergePageItems(current, page.items, (item) => item.messageId));
        setTotal(page.total);
        setCursor(page.nextCursor);
        setStatus("ready");
      })
      .catch((raw: unknown) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setStatus("error-more");
        setError(toAppError(raw));
      });
  }, [api, cursor, query, status]);

  const retry = useCallback(() => {
    setEpoch((value) => value + 1);
  }, []);

  return {
    hits,
    total,
    matchMode,
    note,
    hasMore: cursor !== null,
    status,
    error,
    loadMore,
    retry,
  };
}
