import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import "@/i18n/multicli";
import { multiCliApi } from "./api";
import type { MultiCliApi } from "./api";
import { ErrorPanel } from "./components";
import SessionBrowser from "./SessionBrowser";
import InstallationsView from "./discovery/InstallationsView";
import ProcessesView from "./discovery/ProcessesView";
import type { AppError, DiagnosticsExport } from "./types";
import { toAppError } from "./navigation";

export type AppView = "sessions" | "installations" | "processes";

interface ExportState {
  status: "idle" | "pending" | "done" | "error";
  result?: DiagnosticsExport;
  error?: AppError;
}

const VIEWS: { id: AppView; label: string; testId: string }[] = [
  { id: "sessions", label: "navSessions", testId: "view-sessions" },
  { id: "installations", label: "navInstallations", testId: "view-installations" },
  { id: "processes", label: "navProcesses", testId: "view-processes" },
];

export default function App({ api = multiCliApi }: { api?: MultiCliApi }) {
  const { t } = useTranslation("multicli");
  const [view, setView] = useState<AppView>("sessions");
  const [exportState, setExportState] = useState<ExportState>({ status: "idle" });

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
          className="flex gap-1 rounded-lg bg-background-secondary-default p-1"
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

      {view === "sessions" ? <SessionBrowser api={api} /> : null}
      {view === "installations" ? <InstallationsView api={api} /> : null}
      {view === "processes" ? <ProcessesView api={api} /> : null}
    </div>
  );
}
