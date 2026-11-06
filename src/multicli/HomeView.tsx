import { useCallback, useEffect, useState } from "react";
import { FolderOpen, History, Terminal, ScanLine } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { MultiCliApi } from "./api";
import type { AppError, RuntimeCapabilityView, RuntimeStartInput } from "./types";
import { ErrorPanel } from "./components";
import { toAppError } from "./navigation";
import NewRunComposer from "./runtime/NewRunComposer";

export default function HomeView({ api, visible, busy, onStart, onNavigate }: {
  api: MultiCliApi; visible: boolean; busy: boolean; onStart: (input: RuntimeStartInput) => void;
  onNavigate: (view: "sessions" | "history" | "terminal" | "installations") => void;
}) {
  const { t } = useTranslation("multicli");
  const [capabilities, setCapabilities] = useState<RuntimeCapabilityView[]>([]);
  const [error, setError] = useState<AppError | null>(null);
  const [epoch, setEpoch] = useState(0);
  const retry = useCallback(() => setEpoch(value => value + 1), []);
  useEffect(() => {
    if (!visible) return;
    let current = true;
    void api.runtimeCapabilities().then(result => { if (current) { setCapabilities(result.items); setError(null); } })
      .catch(raw => { if (current) setError(toAppError(raw)); });
    return () => { current = false; };
  }, [api, visible, epoch]);
  return <section className="mc-home" data-testid="home-view">
    <div className="mc-home-inner">
      <p className="mc-home-eyebrow">MultiCLI Workbench</p>
      <h2>{t("homeGreeting")}</h2>
      <p className="mc-home-subtitle">{t("homeSubtitle")}</p>
      {error ? <ErrorPanel error={error} onRetry={retry} scope="home-capabilities" compact /> : null}
      <NewRunComposer capabilities={capabilities} busy={busy || Boolean(error)} onStart={onStart}
        onOpenInstallations={() => onNavigate("installations")} />
      <p className="mc-home-section-label">{t("homeExplore")}</p>
      <div className="mc-home-actions">
        {([
          { view: "sessions", title: "homeProjects", body: "homeProjectsHint", Icon: FolderOpen },
          { view: "history", title: "homeHistory", body: "homeHistoryHint", Icon: History },
          { view: "terminal", title: "navTerminal", body: "homeTerminalHint", Icon: Terminal },
          { view: "installations", title: "navInstallations", body: "homeInstallHint", Icon: ScanLine },
        ] as const).map(({ view, title, body, Icon }) => <button type="button" key={view} onClick={() => onNavigate(view)}>
          <span className={`mc-home-action-icon is-${view}`}><Icon aria-hidden="true" /></span>
          <span><strong>{t(title)}</strong><small>{t(body)}</small></span>
        </button>)}
      </div>
    </div>
    <p className="mc-home-footnote">{t("homeFootnote")}</p>
  </section>;
}
