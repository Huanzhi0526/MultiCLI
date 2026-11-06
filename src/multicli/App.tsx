import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import "@/i18n/multicli";
import { multiCliApi } from "./api";
import type { MultiCliApi } from "./api";
import { ErrorPanel } from "./components";
import SessionBrowser from "./SessionBrowser";
import HomeView from "./HomeView";
import InstallationsView from "./discovery/InstallationsView";
import ProcessesView from "./discovery/ProcessesView";
import HistoryView from "./history/HistoryView";
import TerminalWorkspace from "./terminal/TerminalWorkspace";
import RuntimeView from "./runtime/RuntimeView";
import UsageView from "./usage/UsageView";
import CustomCliView from "./custom/CustomCliView";
import DataView from "./data/DataView";
import type { AppError, DiagnosticsExport, TerminalQuitCheckView, RuntimeStartInput } from "./types";
import { toAppError } from "./navigation";
import { MessagesSquare, History, Terminal, Play, Blocks, Activity, Gauge, SquareTerminal, Database, Plus } from "lucide-react";
import { localTime } from "./presentation";
import "./workbench.css";

export type AppView =
  | "home"
  | "sessions"
  | "history"
  | "installations"
  | "processes"
  | "terminal"
  | "runtime"
  | "usage"
  | "custom"
  | "data";

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
  { id: "data", label: "navData", testId: "view-data" },
];

const QUIT_REQUESTED_EVENT = "multicli://quit-requested";

export default function App({ api = multiCliApi }: { api?: MultiCliApi }) {
  const { t } = useTranslation("multicli");
  const [theme, setTheme] = useState(() => { try { return localStorage.getItem("multicli.theme") || "system"; } catch { return "system"; } });
  const [systemDark, setSystemDark] = useState(() => window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false);
  useEffect(() => {
    const media = window.matchMedia?.("(prefers-color-scheme: dark)");
    const changed = () => setSystemDark(media?.matches ?? false);
    media?.addEventListener("change", changed);
    return () => media?.removeEventListener("change", changed);
  }, []);
  const dark = theme === "dark" || (theme === "system" && systemDark);
  const { i18n } = useTranslation();
  useEffect(() => {
    try { const language = localStorage.getItem("multicli.language"); if (language && ["zh", "en", "ko"].includes(language)) void i18n.changeLanguage(language); } catch { /* Storage is optional. */ }
  }, [i18n]);
  const [view, setView] = useState<AppView>("home");
  const [sidebarHost, setSidebarHost] = useState<HTMLDivElement | null>(null);
  const [runtimeBusy, setRuntimeBusy] = useState(false);
  const [startRequest, setStartRequest] = useState<{id: number; input: RuntimeStartInput} | null>(null);
  const startPending = useRef(false);
  const startSequence = useRef(0);
  const requestNewRun = useCallback((input: RuntimeStartInput) => {
    if (startPending.current || runtimeBusy) return;
    startPending.current = true;
    setStartRequest({id: ++startSequence.current, input});
    setView("runtime");
  }, [runtimeBusy]);
  const handledStartRequest = useCallback(() => { startPending.current = false; setStartRequest(null); }, []);
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
      className={`mc-app flex h-full min-h-0 flex-col bg-background-primary-default text-text-primary ${dark ? "dark" : ""}`}
      data-testid="app-root"
    >
      <header className="mc-header flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-separator-border px-5 py-3">
        <div>
          <h1 className="text-base font-medium tracking-tight">{t(view === "home" ? "navHome" : VIEWS.find(item => item.id === view)!.label)}</h1>
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
                createdAt: localTime(exportState.result.createdAt),
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
          <details className="mc-options">
            <summary data-testid="app-options">{t("appOptions")}</summary>
            <div className="mc-options-panel">
              <div className="mb-4">
                <p className="mb-2 text-xs font-medium">{t("appearance")}</p>
                <div className="flex gap-1 rounded-md bg-background-secondary-default p-1" role="group" aria-label={t("appearance")}>
                  {(["system", "light", "dark"] as const).map(value => <button key={value} type="button" data-testid={`theme-${value}`}
                    aria-pressed={theme === value} className={`flex-1 rounded px-2 py-1.5 text-xs ${theme === value ? "bg-background-primary-default font-medium shadow-sm" : "text-text-secondary"}`}
                    onClick={() => { setTheme(value); try { localStorage.setItem("multicli.theme", value); } catch { /* Applies for this session. */ } }}>
                    {t({ system: "themeSystem", light: "themeLight", dark: "themeDark" }[value])}
                  </button>)}
                </div>
              </div>
              <label>{t("uiLanguage")}<select value={i18n.language} onChange={event => { void i18n.changeLanguage(event.target.value); try { localStorage.setItem("multicli.language", event.target.value); } catch { /* Applies for this session. */ } }}>
                <option value="zh">中文</option><option value="en">English</option><option value="ko">한국어</option>
              </select></label>
              <p className="mb-3 text-xs leading-5 text-text-secondary">{t("diagnosticsHint")}</p>
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
          </details>
        </div>

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

      <div className="mc-body">
        <nav className="mc-nav" aria-label={t("navLabel")}>
          <div className="mc-brand"><span className="mc-brand-mark"><Terminal aria-hidden="true" /></span><strong>MultiCLI</strong><small>Workbench</small></div>
          <button type="button" className="mc-new-chat" data-testid="view-home" aria-current={view === "home" ? "page" : undefined} onClick={() => setView("home")}><Plus aria-hidden="true" /><span>{t("navNewChat")}</span></button>
          {([
            { label: "navWork", ids: ["sessions", "history", "terminal", "runtime"] },
            { label: "navEnvironment", ids: ["installations", "processes", "custom"] },
            { label: "navSystem", ids: ["usage", "data"] },
          ]).map(group => <div className="mc-nav-group" key={group.label}>
            <p>{t(group.label)}</p>
            <div role="tablist" aria-label={t(group.label)}>{group.ids.map(id => {
              const item = VIEWS.find(item => item.id === id)!;
              const Icon = { home: Plus, sessions: MessagesSquare, history: History, terminal: Terminal, runtime: Play, installations: Blocks, processes: Activity, custom: SquareTerminal, usage: Gauge, data: Database }[item.id];
              return <button type="button" key={id} role="tab" aria-selected={view === id} data-testid={item.testId} onClick={() => setView(item.id)}><Icon aria-hidden="true" /><span>{t(item.label)}</span></button>;
            })}</div>
            {group.label === "navWork" ? <div className="mc-scope-slot" ref={setSidebarHost} hidden={view !== "sessions"} /> : null}
          </div>)}
          <p className="mc-nav-footer">{t("tagline")}</p>
        </nav>
        <div className="mc-content">
      <div className={view === "home" ? "flex min-h-0 flex-1 flex-col" : "hidden"}>
        <HomeView api={api} visible={view === "home"} busy={runtimeBusy || Boolean(startRequest)} onStart={requestNewRun} onNavigate={setView} />
      </div>
      {view === "sessions" ? (
        <SessionBrowser
          api={api}
          onResume={requestHistoryResume}
          onNavigate={setView}
          sidebarHost={sidebarHost}
        />
      ) : null}
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
        <TerminalWorkspace api={api} dark={dark} visible={view === "terminal"} />
      </div>
      {/* The runtime view stays mounted for the same reason as the terminal:
          a structured run keeps its connection while another page is shown. */}
      <div
        className={view === "runtime" ? "flex min-h-0 flex-1 flex-col" : "hidden"}
        data-testid="runtime-host"
      >
        <RuntimeView api={api} onOpenInstallations={() => setView("installations")} visible={view === "runtime"} resumeRequest={resumeRequest} onResumeRequestHandled={handledResumeRequest} startRequest={startRequest} onStartRequestHandled={handledStartRequest} onBusyChange={setRuntimeBusy} />
      </div>
      {view === "usage" ? (
        <UsageView api={api} onOpenInstallations={() => setView("installations")} />
      ) : null}
      {view === "custom" ? <CustomCliView api={api} /> : null}
      {view === "data" ? <DataView api={api} /> : null}

        </div>
      </div>

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
