import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { MultiCliApi } from "../api";
import type {
  AppError,
  TerminalExitView,
  TerminalOpenSpec,
  TerminalRunState,
  TerminalStopMode,
  TerminalStopReport,
  TerminalTargetView,
  TerminalTargetsView,
  TerminalView,
} from "../types";
import { ErrorPanel } from "../components";
import { toAppError } from "../navigation";
import TerminalPane from "./TerminalPane";
import {
  CHAIN_LABEL,
  formatBytes,
  isLive,
  parseArgsInput,
  parseEnvInput,
  reduceTabs,
  tabTitle,
  TERMINAL_STATE_LABEL,
} from "./terminalModel";
import type { TabModel } from "./terminalModel";

interface DropNote {
  terminalId: string;
  before: number;
  total: number;
}

const STATUS_POLL_MS = 1200;

export default function TerminalWorkspace({
  api,
  visible = true,
}: {
  api: MultiCliApi;
  visible?: boolean;
}) {
  const { t } = useTranslation("multicli");
  const [model, setModel] = useState<TabModel>({ tabs: [], activeId: null });
  const [targets, setTargets] = useState<TerminalTargetsView | null>(null);
  const [targetsError, setTargetsError] = useState<AppError | null>(null);
  const [views, setViews] = useState<Record<string, TerminalView>>({});
  const [showDialog, setShowDialog] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<AppError | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [drop, setDrop] = useState<DropNote | null>(null);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const loadTargets = useCallback(async () => {
    try {
      const view = await api.terminalTargets();
      if (!mountedRef.current) return;
      setTargets(view);
      setTargetsError(null);
    } catch (raw: unknown) {
      if (!mountedRef.current) return;
      setTargetsError(toAppError(raw));
    }
  }, [api]);

  useEffect(() => {
    void loadTargets();
  }, [loadTargets]);

  const refreshViews = useCallback(async () => {
    try {
      const list = await api.terminalList();
      if (!mountedRef.current) return;
      const next: Record<string, TerminalView> = {};
      for (const item of list.items) next[item.terminalId] = item;
      setViews(next);
      setModel((current) => {
        let nextModel = current;
        for (const item of list.items) {
          nextModel = reduceTabs(nextModel, {
            type: "state",
            terminalId: item.terminalId,
            state: item.state,
            exitCode: item.exit?.code ?? null,
          });
        }
        // A tab the backend no longer knows cannot be driven any more: it is
        // removed instead of staying as a dead control surface. A tab created
        // after this poll started is skipped, so a status snapshot taken before
        // the open cannot delete a brand-new tab.
        const now = Date.now();
        for (const tab of current.tabs) {
          if (next[tab.terminalId]) continue;
          if (now - tab.createdAt < 4000) continue;
          nextModel = reduceTabs(nextModel, {
            type: "remove",
            terminalId: tab.terminalId,
          });
        }
        return nextModel;
      });
    } catch {
      // A failed status poll keeps the last known view: the pane's own read
      // loop already surfaces a real error when the terminal is unreachable.
    }
  }, [api]);

  useEffect(() => {
    if (model.tabs.length === 0) return;
    const timer = window.setInterval(() => {
      void refreshViews();
    }, STATUS_POLL_MS);
    return () => window.clearInterval(timer);
  }, [model.tabs.length, refreshViews]);

  const activeView = model.activeId ? views[model.activeId] : undefined;
  const activeTab = model.tabs.find((tab) => tab.terminalId === model.activeId);

  const handleState = useCallback(
    (terminalId: string, state: TerminalRunState, exit: TerminalExitView | null) => {
      setModel((current) =>
        reduceTabs(current, {
          type: "state",
          terminalId,
          state,
          exitCode: exit?.code ?? null,
        }),
      );
    },
    [],
  );

  const handleDropped = useCallback(
    (terminalId: string, before: number, total: number) => {
      setDrop({ terminalId, before, total });
    },
    [],
  );

  const handleError = useCallback((raw: AppError) => setError(raw), []);

  const create = useCallback(
    async (spec: TerminalOpenSpec) => {
      setBusy(true);
      setError(null);
      try {
        const view = await api.terminalOpen(spec);
        if (!mountedRef.current) return;
        setViews((current) => ({ ...current, [view.terminalId]: view }));
        setModel((current) =>
          reduceTabs(current, {
            type: "add",
            tab: {
              terminalId: view.terminalId,
              runId: view.runId,
              title: view.title,
              chain: view.launchChain,
              state: view.state,
              exitCode: null,
              stopRefused: null,
              createdAt: Date.now(),
            },
          }),
        );
        setShowDialog(false);
        setNotice(null);
      } catch (raw: unknown) {
        setError(toAppError(raw));
      } finally {
        if (mountedRef.current) setBusy(false);
      }
    },
    [api],
  );

  const stop = useCallback(
    async (terminalId: string, mode: TerminalStopMode) => {
      setError(null);
      try {
        const result = await api.terminalStop(terminalId, mode);
        if (!mountedRef.current) return;
        setViews((current) => ({ ...current, [terminalId]: result.terminal }));
        setModel((current) =>
          reduceTabs(current, {
            type: "refused",
            terminalId,
            reason: result.stop.refusedReason,
          }),
        );
        setModel((current) =>
          reduceTabs(current, {
            type: "state",
            terminalId,
            state: result.terminal.state,
            exitCode: result.terminal.exit?.code ?? null,
          }),
        );
        setNotice(describeStop(t, result.stop));
      } catch (raw: unknown) {
        setError(toAppError(raw));
      }
    },
    [api, t],
  );

  const close = useCallback(
    async (terminalId: string) => {
      setError(null);
      try {
        await api.terminalClose(terminalId, "gentle", 1500);
      } catch (raw: unknown) {
        const failure = toAppError(raw);
        // A terminal the backend no longer knows is still safe to forget.
        if (failure.code !== "not_found") {
          setError(failure);
          return;
        }
      }
      if (!mountedRef.current) return;
      setViews((current) => {
        const next = { ...current };
        delete next[terminalId];
        return next;
      });
      setModel((current) => reduceTabs(current, { type: "remove", terminalId }));
      setNotice(null);
      setDrop((current) => (current?.terminalId === terminalId ? null : current));
    },
    [api],
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col" data-testid="terminal-workspace">
      <div className="flex flex-wrap items-center gap-2 border-b border-separator-border px-4 py-2">
        <button
          type="button"
          className="rounded-md border border-separator-border bg-background-primary-default px-3 py-1.5 text-xs hover:bg-background-primary-hover disabled:opacity-60"
          data-testid="terminal-new"
          disabled={busy}
          onClick={() => {
            setError(null);
            void loadTargets();
            setShowDialog(true);
          }}
        >
          {t("terminalNew")}
        </button>
        <button
          type="button"
          className="rounded-md border border-separator-border px-3 py-1.5 text-xs hover:bg-background-primary-hover disabled:opacity-60"
          data-testid="terminal-stop-interrupt"
          disabled={!activeTab || !isLive(activeTab.state)}
          onClick={() => activeTab && void stop(activeTab.terminalId, "interrupt")}
        >
          {t("terminalCtrlC")}
        </button>
        <button
          type="button"
          className="rounded-md border border-separator-border px-3 py-1.5 text-xs hover:bg-background-primary-hover disabled:opacity-60"
          data-testid="terminal-stop-gentle"
          disabled={!activeTab || !isLive(activeTab.state)}
          onClick={() => activeTab && void stop(activeTab.terminalId, "gentle")}
        >
          {t("terminalGentleStop")}
        </button>
        <button
          type="button"
          className="rounded-md border border-separator-border px-3 py-1.5 text-xs hover:bg-background-primary-hover disabled:opacity-60"
          data-testid="terminal-stop-force"
          disabled={!activeTab || !isLive(activeTab.state)}
          onClick={() => activeTab && void stop(activeTab.terminalId, "force")}
        >
          {t("terminalForceStop")}
        </button>
        <button
          type="button"
          className="rounded-md border border-separator-border px-3 py-1.5 text-xs hover:bg-background-primary-hover disabled:opacity-60"
          data-testid="terminal-close"
          disabled={!activeTab}
          onClick={() => activeTab && void close(activeTab.terminalId)}
        >
          {t("terminalCloseTab")}
        </button>
        <span className="ml-auto text-[11px] text-text-tertiary">
          {t("terminalHint", {
            max: targets?.limits.maxTerminals ?? 0,
            buffer: formatBytes(targets?.limits.bufferBytes ?? 0),
            log: formatBytes(targets?.limits.logBytes ?? 0),
          })}
        </span>
      </div>

      {model.tabs.length > 0 ? (
        <div
          className="flex flex-wrap gap-1 border-b border-separator-border bg-background-secondary-default px-3 py-1.5"
          role="tablist"
          aria-label={t("terminalTabs")}
        >
          {model.tabs.map((tab) => (
            <button
              key={tab.terminalId}
              type="button"
              role="tab"
              aria-selected={tab.terminalId === model.activeId}
              data-testid={`terminal-tab-${tab.terminalId}`}
              className={
                tab.terminalId === model.activeId
                  ? "flex items-center gap-2 rounded-md bg-background-primary-default px-3 py-1 text-xs font-medium shadow-sm"
                  : "flex items-center gap-2 rounded-md px-3 py-1 text-xs text-text-secondary hover:text-text-primary"
              }
              onClick={() => {
                // A stop notice belongs to the terminal it was produced for.
                setNotice(null);
                setModel((current) =>
                  reduceTabs(current, { type: "activate", terminalId: tab.terminalId }),
                );
              }}
            >
              <span
                aria-hidden
                className={`h-1.5 w-1.5 rounded-full ${
                  tab.state === "running"
                    ? "bg-state-success-text"
                    : tab.state === "failed"
                      ? "bg-state-error-text"
                      : "bg-text-tertiary"
                }`}
              />
              <span className="max-w-[16rem] truncate">{tab.title}</span>
              <span className="text-[10px] text-text-tertiary">
                {t(TERMINAL_STATE_LABEL[tab.state])}
              </span>
            </button>
          ))}
        </div>
      ) : null}

      {notice ? (
        <p
          className="border-b border-separator-border bg-background-secondary-default px-4 py-1 text-[11px] text-text-secondary"
          data-testid="terminal-notice"
        >
          {notice}
        </p>
      ) : null}

      {drop ? (
        <div
          className="flex items-center gap-3 border-b border-separator-border bg-background-quaternary-warn px-4 py-1 text-[11px] text-text-warning-primary"
          data-testid="terminal-drop-notice"
        >
          <span>
            {t("terminalDropped", {
              before: formatBytes(drop.before),
              total: formatBytes(drop.total),
            })}
          </span>
          <button
            type="button"
            className="underline"
            data-testid="terminal-drop-dismiss"
            onClick={() => setDrop(null)}
          >
            {t("terminalDismiss")}
          </button>
        </div>
      ) : null}

      {error ? (
        <div className="border-b border-separator-border px-4 py-2">
          <ErrorPanel error={error} onRetry={() => setError(null)} scope="terminal" compact />
        </div>
      ) : null}

      <div className="relative min-h-0 flex-1">
        {model.tabs.length === 0 ? (
          <div className="flex h-full flex-col items-center justify-center gap-2 p-8 text-center">
            <p className="text-sm font-medium" data-testid="terminal-empty">
              {t("terminalEmpty")}
            </p>
            <p className="max-w-xl text-xs text-text-tertiary">{t("terminalEmptyHint")}</p>
            <p className="max-w-xl text-[11px] text-text-tertiary">{targets?.note}</p>
          </div>
        ) : null}
        {model.tabs.map((tab) => (
          <div
            key={tab.terminalId}
            className={
              tab.terminalId === model.activeId
                ? "absolute inset-0"
                : "pointer-events-none absolute inset-0 invisible"
            }
          >
            <TerminalPane
              api={api}
              terminalId={tab.terminalId}
              active={visible && tab.terminalId === model.activeId}
              onState={(state, exit) => handleState(tab.terminalId, state, exit)}
              onDropped={(before, total) => handleDropped(tab.terminalId, before, total)}
              onError={handleError}
              onCopyResult={(copied) =>
                setNotice(copied ? t("terminalCopied") : t("terminalCopyFailed"))
              }
            />
          </div>
        ))}
      </div>

      {activeView ? <StatusBar view={activeView} tabTitleText={activeTab?.title ?? ""} /> : null}

      {showDialog ? (
        <NewTerminalDialog
          targets={targets}
          targetsError={targetsError}
          busy={busy}
          onReload={() => void loadTargets()}
          onCancel={() => setShowDialog(false)}
          onCreate={(spec) => void create(spec)}
        />
      ) : null}
    </div>
  );
}

function describeStop(
  t: (key: string, options?: Record<string, unknown>) => string,
  report: TerminalStopReport,
): string {
  if (report.refusedReason) {
    return t("terminalStopRefused", { reason: report.refusedReason });
  }
  if (report.alreadyTerminal) {
    return t("terminalStopAlreadyDone");
  }
  return t("terminalStopDone", {
    mode: t(`terminalStop.${report.requested}`),
    waited: report.waitedMs,
    escalated: report.escalated ? t("terminalStopEscalated") : "",
  });
}

function StatusBar({ view, tabTitleText }: { view: TerminalView; tabTitleText: string }) {
  const { t } = useTranslation("multicli");
  const exit = view.exit;
  const identity = view.identity;
  return (
    <div
      className="grid grid-cols-2 gap-x-6 gap-y-0.5 border-t border-separator-border bg-background-secondary-default px-4 py-1.5 text-[10px] text-text-tertiary"
      data-testid="terminal-status"
    >
      <span>
        {t("terminalStatusState", {
          state: t(TERMINAL_STATE_LABEL[view.state]),
          title: tabTitleText,
        })}
      </span>
      <span data-testid="terminal-status-chain">
        {t("terminalStatusChain", { chain: t(CHAIN_LABEL[view.launchChain]) })}
      </span>
      <span data-testid="terminal-status-run">{t("terminalStatusRun", { run: view.runId })}</span>
      <span data-testid="terminal-status-pid">
        {identity
          ? t("terminalStatusPid", { pid: identity.pid, created: identity.creationTime })
          : t("terminalStatusNoPid")}
      </span>
      <span data-testid="terminal-status-buffer">
        {t("terminalStatusBuffer", {
          buffered: formatBytes(view.buffers.bufferedBytes),
          capacity: formatBytes(view.buffers.bufferCapacityBytes),
          dropped: formatBytes(view.buffers.droppedBytes),
        })}
      </span>
      <span data-testid="terminal-status-log" title={view.buffers.logPath}>
        {t("terminalStatusLog", {
          path: view.buffers.logPath,
          size: formatBytes(view.buffers.logBytes),
          rotations: view.buffers.logRotations,
          dropped: formatBytes(view.buffers.logDroppedBytes),
        })}
      </span>
      <span data-testid="terminal-status-job">
        {view.job.assigned
          ? t("terminalStatusJobOwned")
          : t("terminalStatusJobUnassigned", { reason: view.job.reason ?? "" })}
      </span>
      <span data-testid="terminal-status-exit">
        {exit
          ? t("terminalStatusExit", {
              code: exit.code === null ? t("terminalNoCode") : String(exit.code),
              signal: exit.signal ?? t("terminalNoSignal"),
            })
          : view.state === "exited" || view.state === "failed"
            ? t("terminalStatusNoExit")
            : t("terminalStatusRunning")}
      </span>
      {view.stop ? (
        <span className="col-span-2" data-testid="terminal-status-stop">
          {t("terminalStatusStop", {
            mode: t(`terminalStop.${view.stop.requested}`),
            waited: view.stop.waitedMs,
            escalated: view.stop.escalated ? t("terminalStopEscalated") : t("terminalNoEscalation"),
          })}
        </span>
      ) : null}
    </div>
  );
}

interface DialogProps {
  targets: TerminalTargetsView | null;
  targetsError: AppError | null;
  busy: boolean;
  onReload: () => void;
  onCancel: () => void;
  onCreate: (spec: TerminalOpenSpec) => void;
}

function NewTerminalDialog({
  targets,
  targetsError,
  busy,
  onReload,
  onCancel,
  onCreate,
}: DialogProps) {
  const { t } = useTranslation("multicli");
  const items = useMemo(() => targets?.items ?? [], [targets]);
  const firstReady = items.find((item) => item.status === "ready");
  const [selectedId, setSelectedId] = useState<string>(firstReady?.id ?? "");
  const [cwd, setCwd] = useState(targets?.cwdDefault ?? "");
  const [argsText, setArgsText] = useState("");
  const [envText, setEnvText] = useState("");
  const [localError, setLocalError] = useState<string | null>(null);
  const selected: TerminalTargetView | undefined =
    items.find((item) => item.id === selectedId) ?? firstReady;

  useEffect(() => {
    if (!selectedId && firstReady) setSelectedId(firstReady.id);
  }, [firstReady, selectedId]);

  useEffect(() => {
    if (!cwd && targets?.cwdDefault) setCwd(targets.cwdDefault);
  }, [cwd, targets?.cwdDefault]);

  const submit = () => {
    setLocalError(null);
    if (!selected) {
      setLocalError(t("terminalErrorNoTarget"));
      return;
    }
    if (selected.status !== "ready") {
      setLocalError(selected.reason ?? t("terminalErrorTargetNotReady"));
      return;
    }
    if (!cwd.trim()) {
      setLocalError(t("terminalErrorCwdRequired"));
      return;
    }
    const args = selected.kind === "shell" ? [] : parseArgsInput(argsText);
    const parsedEnv = parseEnvInput(envText);
    if (parsedEnv.invalid.length > 0) {
      setLocalError(t("terminalErrorEnv", { lines: parsedEnv.invalid.join(", ") }));
      return;
    }
    const target =
      selected.kind === "shell"
        ? { kind: "shell" as const, shell: shellOf(selected.id), installationId: null, customCliId: null }
        : selected.kind === "custom-cli"
          ? {
              kind: "custom-cli" as const,
              shell: null,
              installationId: null,
              customCliId: selected.customCliId,
            }
          : {
              kind: "installation" as const,
              shell: null,
              installationId: selected.installationId,
              customCliId: null,
            };
    onCreate({
      target,
      args,
      cwd: cwd.trim(),
      env: parsedEnv.env,
      cols: 120,
      rows: 32,
      title: tabTitle(selected.label, cwd.trim()),
    });
  };

  return (
    <div className="absolute inset-0 z-20 flex items-center justify-center bg-black/30 p-6">
      <div
        className="w-full max-w-2xl rounded-lg border border-separator-border bg-background-primary-default p-4 shadow-lg"
        data-testid="terminal-new-dialog"
      >
        <h2 className="text-sm font-semibold">{t("terminalNewTitle")}</h2>
        <p className="mt-1 text-[11px] text-text-tertiary">{t("terminalNewHint")}</p>

        {targetsError ? (
          <div className="mt-3">
            <ErrorPanel
              error={targetsError}
              onRetry={onReload}
              scope="terminal-targets"
              compact
            />
          </div>
        ) : null}

        <label className="mt-3 block text-[11px] font-medium" htmlFor="terminal-target">
          {t("terminalTargetLabel")}
        </label>
        <select
          id="terminal-target"
          data-testid="terminal-target-select"
          className="mt-1 w-full rounded-md border border-separator-border bg-background-primary-default px-2 py-1.5 text-xs"
          value={selected?.id ?? ""}
          onChange={(event) => setSelectedId(event.target.value)}
        >
          {items.map((item) => (
            <option key={item.id} value={item.id}>
              {item.label}
              {item.status === "ready" ? "" : ` · ${item.status}`}
            </option>
          ))}
        </select>
        {selected?.reason ? (
          <p className="mt-1 text-[11px] text-text-warning-primary" data-testid="terminal-target-reason">
            {selected.reason}
          </p>
        ) : null}
        {selected?.executable ? (
          <p className="mt-1 break-all text-[10px] text-text-tertiary">
            {selected.executable.displayPath}
            {selected.launchChain ? ` · ${t(CHAIN_LABEL[selected.launchChain])}` : ""}
          </p>
        ) : null}

        <label className="mt-3 block text-[11px] font-medium" htmlFor="terminal-cwd">
          {t("terminalCwdLabel")}
        </label>
        <input
          id="terminal-cwd"
          data-testid="terminal-cwd-input"
          className="mt-1 w-full rounded-md border border-separator-border bg-background-primary-default px-2 py-1.5 text-xs"
          value={cwd}
          onChange={(event) => setCwd(event.target.value)}
        />

        <div className="mt-3 grid grid-cols-2 gap-3">
          <div>
            <label className="block text-[11px] font-medium" htmlFor="terminal-args">
              {t("terminalArgsLabel")}
            </label>
            <textarea
              id="terminal-args"
              data-testid="terminal-args-input"
              className="mt-1 h-24 w-full rounded-md border border-separator-border bg-background-primary-default px-2 py-1.5 font-mono text-[11px] disabled:opacity-50"
              disabled={selected?.kind === "shell"}
              value={argsText}
              onChange={(event) => setArgsText(event.target.value)}
              placeholder={t("terminalArgsHint")}
            />
          </div>
          <div>
            <label className="block text-[11px] font-medium" htmlFor="terminal-env">
              {t("terminalEnvLabel")}
            </label>
            <textarea
              id="terminal-env"
              data-testid="terminal-env-input"
              className="mt-1 h-24 w-full rounded-md border border-separator-border bg-background-primary-default px-2 py-1.5 font-mono text-[11px]"
              value={envText}
              onChange={(event) => setEnvText(event.target.value)}
              placeholder="KEY=VALUE"
            />
          </div>
        </div>

        {localError ? (
          <p className="mt-2 text-[11px] text-text-error-primary" data-testid="terminal-dialog-error">
            {localError}
          </p>
        ) : null}

        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            className="rounded-md border border-separator-border px-3 py-1.5 text-xs"
            data-testid="terminal-dialog-cancel"
            onClick={onCancel}
          >
            {t("terminalCancel")}
          </button>
          <button
            type="button"
            className="rounded-md bg-background-primary-default px-3 py-1.5 text-xs shadow-sm ring-1 ring-separator-border disabled:opacity-60"
            data-testid="terminal-dialog-create"
            disabled={busy || !selected}
            onClick={submit}
          >
            {busy ? t("terminalCreating") : t("terminalCreate")}
          </button>
        </div>
      </div>
    </div>
  );
}

function shellOf(id: string): "cmd" | "windows-powershell" | "pwsh" {
  if (id === "shell:windows-powershell") return "windows-powershell";
  if (id === "shell:pwsh") return "pwsh";
  return "cmd";
}
