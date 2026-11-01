import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";

interface BaselineInfo {
  product: string;
  stage: string;
  profile: string;
  dataRoot: string;
  upstreamCommit: string;
  disabled: string[];
}

export default function BaselineApp() {
  const { t } = useTranslation("multicli");
  const [info, setInfo] = useState<BaselineInfo | null>(null);
  const [result, setResult] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  useEffect(() => {
    // Keep the inherited boot watchdog from replacing a healthy mounted page.
    document.documentElement.dataset.appMounted = "1";
    void invoke<BaselineInfo>("multicli_baseline_info").then(setInfo).catch((error: unknown) => setResult(String(error)));
  }, []);
  return <main style={{ maxWidth: 780, margin: "60px auto", padding: 28, fontFamily: "Segoe UI, sans-serif" }}>
    <h1>MultiCLI</h1>
    <p>{t("stage")}</p>
    <dl>
      <dt>{t("profile")}</dt><dd>{info?.profile ?? t("loading")}</dd>
      <dt>{t("dataRoot")}</dt><dd style={{ overflowWrap: "anywhere" }}>{info?.dataRoot ?? t("loading")}</dd>
      <dt>{t("commit")}</dt><dd>{info?.upstreamCommit ?? t("loading")}</dd>
    </dl>
    <p>{t("disabled")}</p>
    <button disabled={pending} onClick={() => {
      setPending(true);
      setResult(null);
      void invoke("multicli_shell_smoke").then((value) => setResult(JSON.stringify(value, null, 2))).catch((error: unknown) => setResult(String(error))).finally(() => setPending(false));
    }}>{t("run")}</button>
    <pre aria-live="polite" style={{ whiteSpace: "pre-wrap" }}>{pending ? t("pending") : result ?? t("notRun")}</pre>
  </main>;
}
