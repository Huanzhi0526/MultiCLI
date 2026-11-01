import { useCallback, useEffect, useRef, useState } from "react";
import { mergePageItems, RequestGate, toAppError } from "./navigation";
import type { AppError, Page } from "./types";

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
