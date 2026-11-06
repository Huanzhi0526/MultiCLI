import { useEffect, useId, useRef, useState } from "react";
import { ArrowUp, FolderOpen, Info } from "lucide-react";
import { useTranslation } from "react-i18next";
import { PathInput, TechnicalDetails } from "../presentation";
import type { RuntimeCapabilityView, RuntimeStartInput } from "../types";
import CliSwitcher from "./CliSwitcher";

export default function NewRunComposer({ capabilities, busy, onStart, onOpenInstallations, compact = false }: {
  capabilities: RuntimeCapabilityView[];
  busy: boolean;
  onStart: (input: RuntimeStartInput) => void;
  onOpenInstallations?: () => void;
  compact?: boolean;
}) {
  const { t } = useTranslation("multicli");
  const [product, setProduct] = useState("codex");
  const [installationId, setInstallationId] = useState("");
  const [cwd, setCwd] = useState("");
  const [prompt, setPrompt] = useState("");
  const [localError, setLocalError] = useState<string | null>(null);
  const chosenByUser = useRef(false);
  const id = useId();
  const candidates = capabilities.filter(item => item.product === product && item.installationId !== null);
  const selected = candidates.find(item => item.installationId === installationId) ?? candidates[0];

  useEffect(() => {
    if (chosenByUser.current) return;
    const first = capabilities.find(item => item.installationId !== null);
    if (first) setProduct(first.product);
  }, [capabilities]);

  const submit = () => {
    if (busy || !selected?.installationId) return;
    if (!cwd.trim()) { setLocalError(t("runtimeErrorCwdRequired")); return; }
    if (!prompt.trim()) { setLocalError(t("runtimeErrorPromptRequired")); return; }
    setLocalError(null);
    onStart({ installationId: selected.installationId, cwd: cwd.trim(), prompt });
  };
  return <div className={`mc-new-composer ${compact ? "is-compact" : ""}`} data-testid="runtime-new-form">
    <CliSwitcher items={capabilities} selected={product} busy={busy} onSelect={value => {
      chosenByUser.current = true;
      setProduct(value); setInstallationId(""); setLocalError(null);
    }} />
    <div className="mc-compose-card">
      <label className="sr-only" htmlFor={`${id}-prompt`}>{t("runtimePromptLabel")}</label>
      <textarea id={`${id}-prompt`} value={prompt}
        data-testid="runtime-prompt-input" placeholder={t("homeComposerPlaceholder")}
        onChange={event => setPrompt(event.target.value)}
        onKeyDown={event => { if (event.key === "Enter" && (event.ctrlKey || event.metaKey) && !event.nativeEvent.isComposing) { event.preventDefault(); submit(); } }} />
      <div className="mc-compose-tools">
        <span className="mc-compose-target"><FolderOpen aria-hidden="true" />{t("cliNewRunScope")}</span>
        <span className="mc-compose-shortcut">Ctrl + Enter</span>
        <button type="button" className="mc-send-button" data-testid="runtime-start"
          disabled={busy || !selected?.installationId || !cwd.trim() || !prompt.trim()}
          title={t("runtimeStart")} aria-label={busy ? t("runtimeStarting") : t("runtimeStart")} onClick={submit}>
          <ArrowUp aria-hidden="true" />
        </button>
      </div>
    </div>
    <div className="mc-compose-project">
      <label className="mc-compose-project-label" htmlFor={`${id}-cwd`}><FolderOpen aria-hidden="true" />{t("homeWorkingFolder")}</label>
      <PathInput id={`${id}-cwd`} testId="runtime-cwd-input" value={cwd} onChange={setCwd} placeholder={"C:\\Work\\project"} />
    </div>
    {!selected ? <p className="mc-compose-availability" role="status"><Info aria-hidden="true" />
      <span>{t("runtimeNotInstalled")}</span>
      {onOpenInstallations ? <button type="button" onClick={onOpenInstallations}>{t("usageOpenInstallations")}</button> : null}
    </p> : null}
    {localError ? <p className="mc-compose-error" data-testid="runtime-form-error" role="alert">{localError}</p> : null}
    {selected ? <TechnicalDetails label={t("cliCapabilities")}>
      <div className="flex flex-wrap items-center gap-2 text-xs">
        <span>{selected.productName} {selected.version}</span>
        {candidates.length > 1 ? <select value={selected.installationId ?? ""} data-testid="runtime-target-select"
          aria-label={t("runtimeTargetLabel")} onChange={event => setInstallationId(event.target.value)}>
          {candidates.map(item => <option key={item.installationId} value={item.installationId ?? ""}>{item.productName} {item.version} · {item.executable?.displayPath}</option>)}
        </select> : null}
        {([ ["runtimeFieldChat", selected.structuredChat], ["runtimeFieldPermission", selected.permissionFlow],
          ["runtimeFieldTools", selected.toolEvents], ["runtimeFieldCancel", selected.cancel], ["runtimeFieldResume", selected.resume] ] as const)
          .map(([label, status]) => <span key={label}>{t(label)} · {t(`capabilityStatus.${status}`)}</span>)}
      </div>
      <p>{t(`capabilityEvidence.${selected.evidence}`, { defaultValue: selected.evidence })} · {selected.reason}</p>
    </TechnicalDetails> : null}
  </div>;
}
