import { useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";

export function localTime(value: string | null | undefined): string {
  if (!value) return "—";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

export function LocalTime({ value }: { value: string | null | undefined }) {
  return <time dateTime={value ?? undefined} title={value ?? undefined}>{localTime(value)}</time>;
}

export function pathName(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).at(-1) || path;
}

export function compactPath(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts.length > 4 ? `${parts.slice(0, 2).join("\\")}\\…\\${parts.slice(-2).join("\\")}` : path;
}

export function PathLabel({ path }: { path: string }) {
  return <span className="block min-w-0" title={path}>
    <span className="block truncate font-medium">{pathName(path)}</span>
    <span className="block truncate text-xs font-normal text-text-tertiary">{compactPath(path)}</span>
  </span>;
}

export function TechnicalDetails({ children, label }: { children: ReactNode; label?: string }) {
  const { t } = useTranslation("multicli");
  return <details className="mc-details">
    <summary>{label ?? t("technicalDetails")}</summary>
    <div className="min-w-0 space-y-2 pt-3">{children}</div>
  </details>;
}

/** Native picker only selects a path. It never launches or reads the selected file. */
export function PathInput({ value, onChange, directory = true, testId, placeholder, id }: {
  value: string; onChange: (value: string) => void; directory?: boolean;
  testId?: string; placeholder?: string; id?: string;
}) {
  const { t } = useTranslation("multicli");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const epoch = useRef(0);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; epoch.current++; };
  }, []);
  const browse = async () => {
    if (busy) return;
    const current = ++epoch.current;
    setBusy(true);
    setError(null);
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const path = await open({ directory, multiple: false, title: t(directory ? "browseFolder" : "browseFile") });
      if (current === epoch.current && typeof path === "string") onChange(path);
    } catch (raw) {
      if (mounted.current && current === epoch.current) setError(t("browseFailed", { detail: String(raw) }));
    } finally { if (mounted.current) setBusy(false); }
  };
  return <span className="block min-w-0">
    <span className="flex min-w-0 gap-2">
      <input id={id} className="min-w-0 flex-1 rounded-md border border-separator-border bg-background-primary-default px-2 py-1.5 text-xs text-text-primary"
        value={value} onChange={event => { epoch.current++; onChange(event.target.value); }} data-testid={testId} placeholder={placeholder} />
      <button type="button" className="shrink-0 rounded-md border border-separator-border px-2 py-1 text-xs" disabled={busy}
        onClick={() => void browse()} data-testid={testId ? `${testId}-browse` : undefined}>{t("browse")}</button>
    </span>
    {error ? <span className="mt-1 block text-xs text-text-error-primary" role="alert">{error}</span> : null}
  </span>;
}

export function messageRole(kind: string): string {
  if (kind.startsWith("user")) return "user";
  if (kind.startsWith("assistant")) return "assistant";
  if (kind.startsWith("tool")) return "tool";
  return "system";
}
