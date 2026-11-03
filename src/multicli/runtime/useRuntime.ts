/**
 * State for the structured runtime view.
 *
 * Live text comes from re-reading the run, not from applying deltas in the
 * WebView. That choice is deliberate: the backend already owns an ordered,
 * de-duplicated journal, so a dropped or reordered notification can only make
 * the view *later*, never wrong. A notification is treated purely as "something
 * changed for this run".
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import type { MultiCliApi } from "../api";
import type {
  AppError,
  RuntimeCapabilitiesView,
  RuntimeRunView,
  RuntimeStartInput,
} from "../types";
import { toAppError } from "../navigation";

const RUNTIME_EVENT = "multicli://runtime";
const REFRESH_DEBOUNCE_MS = 80;
const POLL_MS = 1_500;

export interface RuntimeState {
  capabilities: RuntimeCapabilitiesView | null;
  capabilitiesError: AppError | null;
  runs: RuntimeRunView[];
  activeRunId: string | null;
  run: RuntimeRunView | null;
  busy: boolean;
  error: AppError | null;
  notice: string | null;
}

export function useRuntime(api: MultiCliApi) {
  const [capabilities, setCapabilities] = useState<RuntimeCapabilitiesView | null>(null);
  const [capabilitiesError, setCapabilitiesError] = useState<AppError | null>(null);
  const [runs, setRuns] = useState<RuntimeRunView[]>([]);
  const [activeRunId, setActiveRunId] = useState<string | null>(null);
  const [run, setRun] = useState<RuntimeRunView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<AppError | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const mounted = useRef(true);
  // Mirrors activeRunId synchronously: an awaited response must be judged
  // against the selection *now*, not against the render it was issued in.
  const activeRef = useRef<string | null>(null);
  // Bumped on every deliberate selection (user pick, new run, auto-pick). Used
  // to stop a slow `start`/`resume`/`list` from overriding a later choice.
  const selectEpoch = useRef(0);
  // Per-run request generation. The newest request issued for a run wins; an
  // older response for the same run is discarded instead of regressing it.
  const requestSeq = useRef(new Map<string, number>());
  const pendingRefresh = useRef<number | null>(null);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (pendingRefresh.current !== null) {
        window.clearTimeout(pendingRefresh.current);
      }
    };
  }, []);

  useEffect(() => {
    activeRef.current = activeRunId;
  }, [activeRunId]);

  const beginRequest = useCallback((runId: string): number => {
    const seq = (requestSeq.current.get(runId) ?? 0) + 1;
    requestSeq.current.set(runId, seq);
    return seq;
  }, []);

  /** True when this response is still the newest one for `runId` and we are mounted. */
  const isLatestRequest = useCallback((runId: string, seq: number): boolean => {
    if (!mounted.current) return false;
    return requestSeq.current.get(runId) === seq;
  }, []);

  /**
   * Write a run view back. The list cache is always refreshed; the visible
   * body (transcript / permission cards) only when the view belongs to the
   * run that is active *at write time*.
   */
  const applyRunView = useCallback((view: RuntimeRunView) => {
    setRuns((previous) => {
      const index = previous.findIndex((item) => item.runId === view.runId);
      if (index < 0) return [...previous, view];
      const next = [...previous];
      next[index] = view;
      return next;
    });
    if (activeRef.current !== view.runId) return;
    setRun(view);
  }, []);

  const selectActiveRun = useCallback((runId: string | null) => {
    selectEpoch.current += 1;
    activeRef.current = runId;
    setActiveRunId(runId);
  }, []);

  const loadCapabilities = useCallback(async () => {
    try {
      const view = await api.runtimeCapabilities();
      if (!mounted.current) return;
      setCapabilities(view);
      setCapabilitiesError(null);
    } catch (raw: unknown) {
      if (!mounted.current) return;
      setCapabilitiesError(toAppError(raw));
    }
  }, [api]);

  const refreshRun = useCallback(
    async (runId: string) => {
      const seq = beginRequest(runId);
      try {
        const view = await api.runtimeGet(runId);
        // Superseded by a newer request for this run: its answer is newer.
        if (!isLatestRequest(runId, seq)) return;
        if (view.runId !== runId) {
          // The backend answered about another run; never mix them up.
          return;
        }
        applyRunView(view);
      } catch (raw: unknown) {
        if (!isLatestRequest(runId, seq)) return;
        if (activeRef.current !== runId) return;
        setError(toAppError(raw));
      }
    },
    [api, applyRunView, beginRequest, isLatestRequest],
  );

  const loadRuns = useCallback(async () => {
    const epoch = selectEpoch.current;
    try {
      const view = await api.runtimeList();
      if (!mounted.current) return;
      setRuns(view.items);
      // The user picked something while the list was in flight; their choice
      // already triggered its own refresh, so do not re-elect a run here.
      if (epoch !== selectEpoch.current) return;
      const current = activeRef.current;
      const stillThere = current && view.items.some((item) => item.runId === current);
      if (!stillThere && view.items.length > 0) {
        const last = view.items[view.items.length - 1];
        beginRequest(last.runId);
        selectActiveRun(last.runId);
        setRun(last);
      } else if (current) {
        void refreshRun(current);
      }
    } catch (raw: unknown) {
      if (!mounted.current) return;
      setError(toAppError(raw));
    }
  }, [api, refreshRun, beginRequest, selectActiveRun]);

  useEffect(() => {
    void loadCapabilities();
    void loadRuns();
  }, [loadCapabilities, loadRuns]);

  // A notification only schedules a refresh; it never carries the text.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listen<{ runId: string; seq: number }>(RUNTIME_EVENT, (event) => {
      if (disposed) return;
      const runId = event.payload?.runId ?? activeRef.current;
      if (!runId || runId !== activeRef.current) return;
      if (pendingRefresh.current !== null) return;
      pendingRefresh.current = window.setTimeout(() => {
        pendingRefresh.current = null;
        const target = activeRef.current;
        if (target) void refreshRun(target);
      }, REFRESH_DEBOUNCE_MS);
    })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => {
        // Not running inside Tauri (jsdom tests): the poll below still works.
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refreshRun]);

  // A slow fallback. The notification path is the fast one; this exists so a
  // missed event cannot leave a run frozen on screen.
  useEffect(() => {
    if (!activeRunId) return;
    const timer = window.setInterval(() => {
      if (activeRef.current) void refreshRun(activeRef.current);
    }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [activeRunId, refreshRun]);

  const selectRun = useCallback(
    (runId: string) => {
      selectActiveRun(runId);
      setRun(runs.find((item) => item.runId === runId) ?? null);
      void refreshRun(runId);
    },
    [refreshRun, runs, selectActiveRun],
  );

  const withBusy = useCallback(async <T,>(work: () => Promise<T>): Promise<T | null> => {
    setBusy(true);
    setError(null);
    try {
      return await work();
    } catch (raw: unknown) {
      if (mounted.current) setError(toAppError(raw));
      return null;
    } finally {
      if (mounted.current) setBusy(false);
    }
  }, []);

  /**
   * Mutation responses (`send`/`cancel`/`answer`/`stop`) carry a full run view.
   * They may only touch the visible body while their target is still the
   * active run; otherwise they just refresh that run's row in the list.
   */
  const writeBackMutation = useCallback(
    (view: RuntimeRunView, targetRunId: string, seq: number) => {
      if (!isLatestRequest(targetRunId, seq)) return;
      applyRunView(view);
    },
    [applyRunView, isLatestRequest],
  );

  const activateNewRun = useCallback(
    (view: RuntimeRunView, epoch: number) => {
      setRuns((previous) => [...previous, view]);
      // The activation this call would perform was overtaken by a later
      // selection; keep the run in the list but leave the screen alone.
      if (!mounted.current || epoch !== selectEpoch.current) return;
      beginRequest(view.runId);
      selectActiveRun(view.runId);
      setRun(view);
    },
    [beginRequest, selectActiveRun],
  );

  const start = useCallback(
    async (input: RuntimeStartInput) => {
      const epoch = selectEpoch.current;
      const view = await withBusy(() => api.runtimeStart(input));
      if (!view) return;
      activateNewRun(view, epoch);
      setNotice(null);
    },
    [api, withBusy, activateNewRun],
  );

  const resume = useCallback(
    async (sessionId: string, prompt?: string) => {
      const epoch = selectEpoch.current;
      const view = await withBusy(() => api.runtimeResume({ sessionId, prompt }));
      if (!view) return;
      activateNewRun(view, epoch);
    },
    [api, withBusy, activateNewRun],
  );

  const send = useCallback(
    async (text: string) => {
      const target = activeRef.current;
      if (!target) return;
      const seq = beginRequest(target);
      const view = await withBusy(() => api.runtimeSend(target, text));
      if (view) writeBackMutation(view, target, seq);
    },
    [api, withBusy, beginRequest, writeBackMutation],
  );

  const cancel = useCallback(async () => {
    const target = activeRef.current;
    if (!target) return;
    const seq = beginRequest(target);
    const view = await withBusy(() => api.runtimeCancel(target));
    if (view) writeBackMutation(view, target, seq);
  }, [api, withBusy, beginRequest, writeBackMutation]);

  const answer = useCallback(
    async (permissionId: string, optionId: string) => {
      const target = activeRef.current;
      if (!target) return;
      const seq = beginRequest(target);
      const view = await withBusy(() =>
        api.runtimeAnswer({ runId: target, permissionId, optionId }),
      );
      if (view) writeBackMutation(view, target, seq);
    },
    [api, withBusy, beginRequest, writeBackMutation],
  );

  const stop = useCallback(async () => {
    const target = activeRef.current;
    if (!target) return;
    const seq = beginRequest(target);
    const view = await withBusy(() => api.runtimeStop(target));
    if (view) writeBackMutation(view, target, seq);
  }, [api, withBusy, beginRequest, writeBackMutation]);

  const state: RuntimeState = {
    capabilities,
    capabilitiesError,
    runs,
    activeRunId,
    run,
    busy,
    error,
    notice,
  };

  return {
    ...state,
    reload: loadRuns,
    reloadCapabilities: loadCapabilities,
    selectRun,
    start,
    resume,
    send,
    cancel,
    answer,
    stop,
    clearError: () => setError(null),
    setNotice,
  };
}
