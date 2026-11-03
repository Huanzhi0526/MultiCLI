import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { MultiCliApi } from "../api";
import { Badge, ErrorPanel, LoadingPanel } from "../components";
import type { AppError, CustomCliView } from "../types";
import { toAppError } from "../navigation";

interface FormState {
  label: string;
  executable: string;
  argsText: string;
  cwd: string;
  productHint: string;
}

const EMPTY: FormState = {
  label: "",
  executable: "",
  argsText: "",
  cwd: "",
  productHint: "",
};

/** One argument per line: quotes, spaces, leading spaces and Chinese stay
 *  literal, because the backend receives an array and never re-parses a command
 *  line. Only completely blank lines are dropped. */
export function parseArgs(text: string): string[] {
  return text.split(/\r?\n/).filter((line) => line.trim().length > 0);
}

export default function CustomCliView({ api }: { api: MultiCliApi }) {
  const { t } = useTranslation("multicli");
  const [items, setItems] = useState<CustomCliView[]>([]);
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [error, setError] = useState<AppError | null>(null);
  const [form, setForm] = useState<FormState>(EMPTY);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [deleteError, setDeleteError] = useState<AppError | null>(null);
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);

  const load = useCallback(async () => {
    setStatus("loading");
    try {
      const result = await api.customCliList();
      setItems(result);
      setStatus("ready");
      setError(null);
    } catch (raw: unknown) {
      setError(toAppError(raw));
      setStatus("error");
    }
  }, [api]);

  useEffect(() => {
    void load();
  }, [load]);

  const submit = useCallback(async () => {
    setBusy(true);
    setNotice(null);
    try {
      await api.customCliCreate({
        label: form.label,
        executable: form.executable,
        args: parseArgs(form.argsText),
        cwd: form.cwd.trim() ? form.cwd.trim() : null,
        productHint: form.productHint.trim() ? form.productHint.trim() : null,
      });
      setForm(EMPTY);
      await load();
      setNotice(t("customCliCreated"));
    } catch (raw: unknown) {
      setError(toAppError(raw));
    } finally {
      setBusy(false);
    }
  }, [api, form, load, t]);

  const remove = useCallback(
    async (id: string) => {
      if (pendingDelete !== id) {
        // Deleting a preset is a local, reversible-by-recreating action but it
        // still deserves one confirmation click.
        setPendingDelete(id);
        setDeleteError(null);
        return;
      }
      setBusy(true);
      setDeleteError(null);
      try {
        await api.customCliDelete(id);
        setPendingDelete(null);
        await load();
        setNotice(t("customCliDeleted"));
      } catch (raw: unknown) {
        // Deleting must not reuse the submit error path (whose retry would
        // create a preset).
        setDeleteError(toAppError(raw));
      } finally {
        setBusy(false);
      }
    },
    [api, load, pendingDelete, t],
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col" data-testid="custom-cli-view">
      <div className="flex flex-wrap items-center gap-2 border-b border-separator-border px-5 py-2">
        <h2 className="text-sm font-semibold">{t("customCliTitle")}</h2>
        <span className="text-[11px] text-text-tertiary">{t("customCliCount", { count: items.length })}</span>
        <button
          type="button"
          className="ml-auto rounded-md border border-separator-border px-3 py-1 text-[11px] text-text-secondary hover:bg-background-primary-hover"
          onClick={() => void load()}
          data-testid="custom-cli-reload"
        >
          {t("refresh")}
        </button>
      </div>
      <p className="border-b border-separator-border px-5 py-2 text-[10px] leading-4 text-text-tertiary">
        {t("customCliHint")}
      </p>

      <div className="min-h-0 flex-1 overflow-auto px-5 pb-8">
        {status === "loading" ? <LoadingPanel label={t("loading")} /> : null}
        {status === "error" && error ? (
          <ErrorPanel error={error} onRetry={() => void load()} scope="custom-cli" />
        ) : null}
        {deleteError ? (
          <div className="mt-3">
            <ErrorPanel
              error={deleteError}
              onRetry={() => setDeleteError(null)}
              scope="custom-cli-delete"
              compact
            />
          </div>
        ) : null}

        <section className="mt-3 rounded-lg border border-separator-border px-4 py-3">
          <h3 className="text-xs font-semibold text-text-primary">{t("customCliFormTitle")}</h3>
          <div className="mt-2 grid gap-2 sm:grid-cols-2">
            <label className="flex flex-col gap-1 text-[10px] uppercase tracking-wide text-text-tertiary">
              {t("customCliLabel")}
              <input
                className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 text-xs text-text-primary"
                value={form.label}
                onChange={(event) => setForm({ ...form, label: event.target.value })}
                data-testid="custom-cli-label"
              />
            </label>
            <label className="flex flex-col gap-1 text-[10px] uppercase tracking-wide text-text-tertiary">
              {t("customCliExecutable")}
              <input
                className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 font-mono text-xs text-text-primary"
                value={form.executable}
                onChange={(event) => setForm({ ...form, executable: event.target.value })}
                placeholder="C:\\path\\to\\tool.exe"
                data-testid="custom-cli-executable"
              />
            </label>
            <label className="flex flex-col gap-1 text-[10px] uppercase tracking-wide text-text-tertiary">
              {t("customCliCwd")}
              <input
                className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 font-mono text-xs text-text-primary"
                value={form.cwd}
                onChange={(event) => setForm({ ...form, cwd: event.target.value })}
                placeholder="D:\\Work"
                data-testid="custom-cli-cwd"
              />
            </label>
            <label className="flex flex-col gap-1 text-[10px] uppercase tracking-wide text-text-tertiary">
              {t("customCliProductHint")}
              <input
                className="rounded-md border border-separator-border bg-background-primary-default px-2 py-1 text-xs text-text-primary"
                value={form.productHint}
                onChange={(event) => setForm({ ...form, productHint: event.target.value })}
                data-testid="custom-cli-product"
              />
            </label>
            <label className="flex flex-col gap-1 text-[10px] uppercase tracking-wide text-text-tertiary sm:col-span-2">
              {t("customCliArgs")}
              <textarea
                className="h-24 rounded-md border border-separator-border bg-background-primary-default px-2 py-1 font-mono text-xs text-text-primary"
                value={form.argsText}
                onChange={(event) => setForm({ ...form, argsText: event.target.value })}
                placeholder={"--version\n中文 参数"}
                data-testid="custom-cli-args"
              />
            </label>
          </div>
          <p className="mt-1 text-[10px] text-text-tertiary">{t("customCliArgsHint")}</p>
          <div className="mt-2 flex items-center gap-2">
            <button
              type="button"
              className="rounded-md border border-separator-border bg-background-primary-default px-3 py-1 text-[11px] shadow-sm disabled:opacity-60"
              onClick={() => void submit()}
              disabled={busy || !form.label.trim() || !form.executable.trim()}
              data-testid="custom-cli-submit"
            >
              {busy ? t("customCliSaving") : t("customCliSave")}
            </button>
            {notice ? (
              <span className="text-[10px] text-state-success-text" data-testid="custom-cli-notice">
                {notice}
              </span>
            ) : null}
          </div>
          {error && status !== "error" ? (
            <div className="mt-2">
              <ErrorPanel error={error} onRetry={() => void submit()} scope="custom-cli-submit" compact />
            </div>
          ) : null}
        </section>

        {status === "ready" && items.length === 0 ? (
          <p className="mt-3 text-[11px] text-text-tertiary" data-testid="custom-cli-empty">
            {t("customCliEmpty")}
          </p>
        ) : null}

        <ul className="mt-3 flex flex-col gap-2">
          {items.map((item) => (
            <li
              key={item.id}
              className="rounded-lg border border-separator-border px-4 py-3"
              data-testid={`custom-cli-row-${item.id}`}
            >
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-sm font-medium text-text-primary">{item.label}</span>
                <Badge tone="blue">{item.launchChain}</Badge>
                <Badge tone="neutral">{item.verification}</Badge>
                <code className="text-[10px] text-text-tertiary">{item.id}</code>
                <button
                  type="button"
                  className={
                    pendingDelete === item.id
                      ? "ml-auto rounded-md border border-state-danger-text px-2 py-0.5 text-[10px] text-state-danger-text hover:bg-background-primary-hover disabled:opacity-60"
                      : "ml-auto rounded-md border border-separator-border px-2 py-0.5 text-[10px] text-state-danger-text hover:bg-background-primary-hover disabled:opacity-60"
                  }
                  onClick={() => void remove(item.id)}
                  onBlur={() => setPendingDelete(null)}
                  disabled={busy}
                  data-testid={`custom-cli-delete-${item.id}`}
                >
                  {pendingDelete === item.id ? t("customCliConfirmDelete") : t("customCliDelete")}
                </button>
              </div>
              <p className="mt-1 font-mono text-[11px] break-all text-text-secondary">
                {item.executable.displayPath}
              </p>
              <p className="mt-0.5 font-mono text-[10px] break-all text-text-tertiary">
                argv: {item.args.length === 0 ? "—" : item.args.join(" · ")}
              </p>
              <p className="mt-0.5 text-[10px] text-text-tertiary">
                {t("customCliCwdValue")}: {item.cwd ?? "—"} · {t("customCliAccess")}:{" "}
                {item.historyAccess} / {item.quotaAccess}
              </p>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
