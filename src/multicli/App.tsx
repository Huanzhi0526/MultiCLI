import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import "@/i18n/multicli";
import { multiCliApi } from "./api";
import type { MultiCliApi } from "./api";
import { ErrorPanel } from "./components";
import SessionBrowser from "./SessionBrowser";
import InstallationsView from "./discovery/InstallationsView";
import ProcessesView from "./discovery/ProcessesView";
import HistoryView from "./history/HistoryView";
import TerminalWorkspace from "./terminal/TerminalWorkspace";
import RuntimeView from "./runtime/RuntimeView";
import UsageView from "./usage/UsageView";
import CustomCliView from "./custom/CustomCliView";
import type { AppError, DiagnosticsExport, TerminalQuitCheckView } from "./types";
import { toAppError } from "./navigation";

export type AppView =
  | "sessions"
  | "history"
  | "installations"
  | "processes"
  | "terminal"
  | "runtime"
  | "usage"
  | "custom";

interface ExportState {
  status: "idle" | "pending" | "done" | "error";
  result?: DiagnosticsExport;
  error?: AppError;
}

const VIEWS: { id: AppView; label: string; testId: string }[] = [
  { id: "sessions", label: "navSessions", testId: "view-sessions" },
  { id: "history", label: "navHistory", testId: "view-history" },
  { id: "installations", label: "navInstallations", testId: "view-installations" },
  { id: "processes", label: "navProcesses", testId: "view-processes" },
  { id: "terminal", label: "navTerminal", testId: "view-terminal" },
  { id: "runtime", label: "navRuntime", testId: "view-runtime" },
  { id: "usage", label: "navUsage", testId: "view-usage" },
  { id: "custom", label: "navCustomCli", testId: "view-custom-cli" },
];

const QUIT_REQUESTED_EVENT = "multicli://quit-requested";

export default function App({ api = multiCliApi }: { api?: MultiCliApi }) {
  const { t } = useTranslation("multicli");
  const [view, setView] = useState<AppView>("sessions");
  const [exportState, setExportState] = useState<ExportState>({ status: "idle" });
  const [quit, setQuit] = useState<TerminalQuitCheckView | null>(null);
  const [quitError, setQuitError] = useState<AppError | null>(null);
  const [resumeRequest, setResumeRequest] = useState<{id: number; sessionId: string} | null>(null);
  const resumeSequence = useRef(0);
  const requestHistoryResume = useCallback((sessionId: string) => {
    setResumeRequest({id: ++resumeSequence.current, sessionId});
    setView("runtime");
  }, []);
  const handledResumeRequest = useCallback(() => setResumeRequest(null), []);

  useEffect(() => {
    // Keep the inherited boot watchdog from replacing a healthy mounted page.
    document.documentElement.dataset.appMounted = "1";
  }, []);

  const exportDiagnostics = useCallback(async () => {
    setExportState({ status: "pending" });
    try {
      const result = await api.exportDiagnostics();
      setExportState({ status: "done", result });
    } catch (raw: unknown) {
      setExportState({ status: "error", error: toAppError(raw) });
    }
  }, [api]);

  // The tray asks before anything is stopped. With no managed terminal the
  // application exits straight away; otherwise the window shows the explicit
  // cancel-or-stop choice and never kills a process by itself.
  const requestQuit = useCallback(() => {
    void api
      .terminalQuitCheck()
      .then((check) => {
        if (!check.needsConfirmation) {
          void api.appQuit().catch((raw: unknown) => setQuitError(toAppError(raw)));
          return;
        }
        setQuit(check);
      })
      .catch((raw: unknown) => {
        // Without a status check the only safe action is to show the
        // confirmation instead of quitting blindly.
        setQuit({
          activeCount: 0,
          needsConfirmation: true,
          note: toAppError(raw).message,
        });
      });
  }, [api]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listen(QUIT_REQUESTED_EVENT, () => {
      if (!disposed) requestQuit();
    })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => {
        // Not running inside Tauri (jsdom tests): no tray can ask.
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [requestQuit]);

  const confirmQuit = useCallback(async () => {
    setQuitError(null);
    try {
      await api.appQuit();
    } catch (raw: unknown) {
      setQuitError(toAppError(raw));
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
        <nav
          className="flex flex-wrap gap-1 rounded-lg bg-background-secondary-default p-1"
          role="tablist"
          aria-label={t("navLabel")}
        >
          {VIEWS.map((item) => (
            <button
              key={item.id}
              type="button"
              role="tab"
              aria-selected={view === item.id}
              className={
                view === item.id
                  ? "rounded-md bg-background-primary-default px-3 py-1.5 text-xs font-medium text-text-primary shadow-sm"
                  : "rounded-md px-3 py-1.5 text-xs font-medium text-text-secondary hover:text-text-primary"
              }
              data-testid={item.testId}
              onClick={() => setView(item.id)}
            >
              {t(item.label)}
            </button>
          ))}
        </nav>
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
          {/* Same path as the tray "退出": never quits without an explicit
              confirmation while this application still owns a terminal. */}
          <button
            type="button"
            className="rounded-md border border-separator-border bg-background-primary-default px-3 py-1.5 text-xs hover:bg-background-primary-hover"
            onClick={requestQuit}
            data-testid="app-quit"
          >
            {t("quitEntry")}
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

      {view === "sessions" ? <SessionBrowser api={api} onResume={requestHistoryResume} /> : null}
      {view === "history" ? <HistoryView api={api} onResume={requestHistoryResume} /> : null}
      {view === "installations" ? <InstallationsView api={api} /> : null}
      {view === "processes" ? <ProcessesView api={api} /> : null}
      {/* The terminal workspace stays mounted while another page is shown:
          unmounting it would drop every tab while the PTYs keep running, and the
          user would have no way back to a terminal this application still owns. */}
      <div
        className={view === "terminal" ? "flex min-h-0 flex-1 flex-col" : "hidden"}
        data-testid="terminal-host"
      >
        <TerminalWorkspace api={api} visible={view === "terminal"} />
      </div>
      {/* The runtime view stays mounted for the same reason as the terminal:
          a structured run keeps its connection while another page is shown. */}
      <div
        className={view === "runtime" ? "flex min-h-0 flex-1 flex-col" : "hidden"}
        data-testid="runtime-host"
      >
        <RuntimeView api={api} visible={view === "runtime"} resumeRequest={resumeRequest} onResumeRequestHandled={handledResumeRequest} />
      </div>
      {view === "usage" ? <UsageView api={api} /> : null}
      {view === "custom" ? <CustomCliView api={api} /> : null}

      {quit ? (
        <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/40 p-6">
          <div
            className="w-full max-w-lg rounded-lg border border-separator-border bg-background-primary-default p-4 shadow-lg"
            data-testid="quit-dialog"
          >
            <h2 className="text-sm font-semibold">{t("quitTitle")}</h2>
            <p className="mt-2 text-xs text-text-secondary">
              {t("quitBody", { count: quit.activeCount })}
            </p>
            <p className="mt-1 text-[11px] text-text-tertiary">{quit.note}</p>
            {quitError ? (
              <div className="mt-2">
                <ErrorPanel
                  error={quitError}
                  onRetry={() => void confirmQuit()}
                  scope="quit"
                  compact
                />
              </div>
            ) : null}
            <div className="mt-4 flex justify-end gap-2">
              <button
                type="button"
                className="rounded-md border border-separator-border px-3 py-1.5 text-xs"
                data-testid="quit-cancel"
                onClick={() => setQuit(null)}
              >
                {t("quitCancel")}
              </button>
              <button
                type="button"
                className="rounded-md bg-background-primary-default px-3 py-1.5 text-xs shadow-sm ring-1 ring-separator-border"
                data-testid="quit-confirm"
                onClick={() => void confirmQuit()}
              >
                {t("quitConfirm")}
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
