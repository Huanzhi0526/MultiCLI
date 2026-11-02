import { useCallback, useEffect, useRef, useState } from "react";
import { mergePageItems, RequestGate, toAppError } from "./navigation";
import type {
  AppError,
  HistoryProvenanceView,
  Message,
  MessagePage,
  MessagePageQuery,
  Page,
  Run,
  Session,
  SessionMetadataView,
  SessionWorkspace,
} from "./types";

export type LoadStatus = "idle" | "loading" | "ready" | "error" | "loading-more" | "error-more";

interface PagedState<T> {
  items: T[];
  total: number;
  cursor: string | null;
  status: LoadStatus;
  error: AppError | null;
}

export interface PagedResource<T> extends PagedState<T> {
  loadMore: () => void;
  refresh: () => void;
}

const EMPTY_STATE = {
  items: [],
  total: 0,
  cursor: null,
  status: "idle" as LoadStatus,
  error: null,
};

/**
 * Loads page 0 whenever `key` changes and appends further pages with the
 * returned cursor. `key` must encode every filter so a filter change resets
 * the cursor; responses that arrive after a newer request are ignored.
 */
export function usePagedResource<T>(
  key: string | null,
  load: (cursor: string | null) => Promise<Page<T>>,
  idOf: (item: T) => string,
): PagedResource<T> {
  const [state, setState] = useState<PagedState<T>>(EMPTY_STATE);
  const [epoch, setEpoch] = useState(0);
  const gateRef = useRef<RequestGate | null>(null);
  if (!gateRef.current) {
    gateRef.current = new RequestGate();
  }
  const loadRef = useRef(load);
  loadRef.current = load;
  const idOfRef = useRef(idOf);
  idOfRef.current = idOf;
  const previousKeyRef = useRef<string | null>(null);
  const previousEpochRef = useRef(0);

  useEffect(() => {
    const gate = gateRef.current as RequestGate;
    if (key === null) {
      gate.invalidate();
      previousKeyRef.current = null;
      previousEpochRef.current = epoch;
      setState(EMPTY_STATE);
      return;
    }
    // A changed filter starts a new list; an epoch-only re-run is a refresh of
    // the same list, so keep old rows visible until the fresh page arrives.
    const isRefresh =
      previousKeyRef.current === key && previousEpochRef.current !== epoch;
    previousKeyRef.current = key;
    previousEpochRef.current = epoch;
    const token = gate.begin();
    setState((current) => {
      if (isRefresh && current.items.length > 0) {
        return { ...current, status: "loading", error: null };
      }
      return { ...EMPTY_STATE, status: "loading" };
    });
    void loadRef
      .current(null)
      .then((page) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setState({
          items: mergePageItems([], page.items, idOfRef.current),
          total: page.total,
          cursor: page.nextCursor,
          status: "ready",
          error: null,
        });
      })
      .catch((raw: unknown) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        // Keep any previously shown rows on a refresh failure; the error panel
        // sits above them and the user can retry.
        setState((current) => ({
          ...current,
          status: "error",
          error: toAppError(raw),
        }));
      });
    return () => {
      gate.invalidate();
    };
  }, [key, epoch]);

  const loadMore = useCallback(() => {
    const gate = gateRef.current as RequestGate;
    if (
      (state.status !== "ready" && state.status !== "error-more") ||
      !state.cursor
    ) {
      return;
    }
    const token = gate.begin();
    const cursor = state.cursor;
    setState((current) => ({ ...current, status: "loading-more", error: null }));
    void loadRef
      .current(cursor)
      .then((page) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setState((current) => ({
          items: mergePageItems(current.items, page.items, idOfRef.current),
          total: page.total,
          cursor: page.nextCursor,
          status: "ready",
          error: null,
        }));
      })
      .catch((raw: unknown) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setState((current) => ({
          ...current,
          status: "error-more",
          error: toAppError(raw),
        }));
      });
  }, [state.cursor, state.status]);

  const refresh = useCallback(() => {
    setEpoch((value) => value + 1);
  }, []);

  return { ...state, loadMore, refresh };
}

interface DetailState<T> {
  data: T | null;
  status: "idle" | "loading" | "ready" | "error";
  error: AppError | null;
}

export interface DetailResource<T> extends DetailState<T> {
  refresh: () => void;
}

/** Loads one entity and drops late responses from a previous selection. */
export function useDetailResource<T>(
  id: string | null,
  load: (id: string) => Promise<T>,
): DetailResource<T> {
  const [state, setState] = useState<DetailState<T>>({
    data: null,
    status: "idle",
    error: null,
  });
  const [epoch, setEpoch] = useState(0);
  const gateRef = useRef<RequestGate | null>(null);
  if (!gateRef.current) {
    gateRef.current = new RequestGate();
  }
  const loadRef = useRef(load);
  loadRef.current = load;

  useEffect(() => {
    const gate = gateRef.current as RequestGate;
    if (!id) {
      gate.invalidate();
      setState({ data: null, status: "idle", error: null });
      return;
    }
    const token = gate.begin();
    setState({ data: null, status: "loading", error: null });
    void loadRef
      .current(id)
      .then((data) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setState({ data, status: "ready", error: null });
      })
      .catch((raw: unknown) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setState({ data: null, status: "error", error: toAppError(raw) });
      });
    return () => {
      gate.invalidate();
    };
  }, [id, epoch]);

  const refresh = useCallback(() => {
    setEpoch((value) => value + 1);
  }, []);

  return { ...state, refresh };
}

/**
 * A session opened from any entry (workspace, agent or search) is read through
 * metadata plus a bounded message page.
 *
 * `getSession` returns every message of a session in one response, which is
 * unbounded for a history transcript with tens of thousands of rows. The view
 * model below is built from `getSessionMetadata` and `listSessionMessages`, so
 * the same session id, counts and message order are shown everywhere while the
 * payload stays capped at one page.
 */
export const SESSION_DETAIL_PAGE_SIZE = 100;

export interface SessionDetailViewData {
  session: Session;
  workspaces: SessionWorkspace[];
  runs: Run[];
  messages: Message[];
  messageCount: number;
  runCount: number;
  history: HistoryProvenanceView | null;
}

export interface SessionDetailResource {
  data: SessionDetailViewData | null;
  status: "idle" | "loading" | "ready" | "error";
  error: AppError | null;
  hasMore: boolean;
  loadingMore: boolean;
  moreError: AppError | null;
  loadMore: () => void;
  refresh: () => void;
}

interface SessionDetailState {
  data: SessionDetailViewData | null;
  status: "idle" | "loading" | "ready" | "error";
  error: AppError | null;
  cursor: string | null;
  loadingMore: boolean;
  moreError: AppError | null;
}

const EMPTY_DETAIL: SessionDetailState = {
  data: null,
  status: "idle",
  error: null,
  cursor: null,
  loadingMore: false,
  moreError: null,
};

export interface SessionDetailApi {
  getSessionMetadata(sessionId: string): Promise<SessionMetadataView>;
  listSessionMessages(query: MessagePageQuery): Promise<MessagePage>;
}

export function useSessionDetail(
  api: SessionDetailApi,
  sessionId: string | null,
): SessionDetailResource {
  const [state, setState] = useState<SessionDetailState>(EMPTY_DETAIL);
  const [epoch, setEpoch] = useState(0);
  const gateRef = useRef<RequestGate | null>(null);
  if (!gateRef.current) {
    gateRef.current = new RequestGate();
  }
  const apiRef = useRef(api);
  apiRef.current = api;
  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(() => {
    const gate = gateRef.current as RequestGate;
    if (!sessionId) {
      gate.invalidate();
      setState(EMPTY_DETAIL);
      return;
    }
    const token = gate.begin();
    setState({ ...EMPTY_DETAIL, status: "loading" });
    void Promise.all([
      apiRef.current.getSessionMetadata(sessionId),
      apiRef.current.listSessionMessages({
        sessionId,
        limit: SESSION_DETAIL_PAGE_SIZE,
      }),
    ])
      .then(([metadata, page]) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setState({
          data: buildSessionDetail(metadata, page.items),
          status: "ready",
          error: null,
          cursor: page.nextCursor,
          loadingMore: false,
          moreError: null,
        });
      })
      .catch((raw: unknown) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setState({ ...EMPTY_DETAIL, status: "error", error: toAppError(raw) });
      });
    return () => {
      gate.invalidate();
    };
  }, [sessionId, epoch]);

  const loadMore = useCallback(() => {
    const current = stateRef.current;
    if (
      !sessionId ||
      current.status !== "ready" ||
      !current.cursor ||
      current.loadingMore
    ) {
      return;
    }
    const gate = gateRef.current as RequestGate;
    const token = gate.begin();
    const cursor = current.cursor;
    setState((value) => ({ ...value, loadingMore: true, moreError: null }));
    void apiRef.current
      .listSessionMessages({
        sessionId,
        limit: SESSION_DETAIL_PAGE_SIZE,
        cursor,
      })
      .then((page) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setState((value) => ({
          ...value,
          data: value.data
            ? {
                ...value.data,
                messages: mergePageItems(
                  value.data.messages,
                  page.items,
                  (message) => message.id,
                ),
              }
            : value.data,
          cursor: page.nextCursor,
          loadingMore: false,
          moreError: null,
        }));
      })
      .catch((raw: unknown) => {
        if (!gate.isCurrent(token)) {
          return;
        }
        setState((value) => ({
          ...value,
          loadingMore: false,
          moreError: toAppError(raw),
        }));
      });
  }, [sessionId]);

  const refresh = useCallback(() => {
    setEpoch((value) => value + 1);
  }, []);

  return {
    data: state.data,
    status: state.status,
    error: state.error,
    hasMore: state.cursor !== null,
    loadingMore: state.loadingMore,
    moreError: state.moreError,
    loadMore,
    refresh,
  };
}

function buildSessionDetail(
  metadata: SessionMetadataView,
  messages: Message[],
): SessionDetailViewData {
  return {
    session: metadata.session,
    workspaces: metadata.workspaces.map((link) => ({
      sessionId: metadata.session.id,
      workspaceId: link.workspaceId,
      role: link.role,
      evidence: link.evidence,
    })),
    runs: metadata.runs,
    messages,
    messageCount: metadata.messageCount,
    runCount: metadata.runTotal,
    history: metadata.history,
  };
}
