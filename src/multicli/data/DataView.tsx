import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { MultiCliApi } from "../api";
import { Badge, ErrorPanel, LoadingPanel } from "../components";
import type { AppError, BackupRecord, ProfileDataView } from "../types";
import { toAppError } from "../navigation";

function bytesText(value: number): string {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
  if (value < 1024 * 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(value / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
}

function localTime(value: string | null): string {
  if (!value) return "—";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString();
}

function integrityTone(integrity: string): "green" | "amber" | "red" | "neutral" {
  if (integrity === "ok") return "green";
  if (integrity === "missing") return "red";
  if (
    integrity === "checksum-mismatch" ||
    integrity === "unreadable" ||
    integrity === "invalid-name"
  )
    return "amber";
  return "neutral";
}

/**
 * Own-profile data: what this application stores, how to back it up, and how a
 * restore is applied. The snapshot contains this app's indexed sessions and
 * messages, but never CLI credential files or CLI-native history files.
 */
export default function DataView({ api }: { api: MultiCliApi }) {
  const { t } = useTranslation("multicli");
  const [data, setData] = useState<ProfileDataView | null>(null);
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [error, setError] = useState<AppError | null>(null);
  const [actionError, setActionError] = useState<AppError | null>(null);
  const [busy, setBusy] = useState<"backup" | "restore" | "cancel" | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<BackupRecord | null>(null);

  const load = useCallback(async () => {
    // A refresh failure keeps the rows already on screen; only the very first
    // load uses the full-page loading/error states.
    try {
      const result = await api.profileData();
      setData(result);
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

  useEffect(() => {
    if (!confirm) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && busy === null) setConfirm(null);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [confirm, busy]);

  const backup = useCallback(async () => {
    setBusy("backup");
    setActionError(null);
    setMessage(null);
    try {
      const record = await api.profileBackup();
      setMessage(t("dataBackupDone", { file: record.manifest.fileName }));
      await load();
    } catch (raw: unknown) {
      setActionError(toAppError(raw));
    } finally {
      setBusy(null);
    }
  }, [api, load, t]);

  const planRestore = useCallback(async () => {
    if (!confirm) return;
    setBusy("restore");
    setActionError(null);
    setMessage(null);
    try {
      await api.profileRestore(confirm.manifest.fileName);
      setMessage(t("dataRestorePlanned"));
      setConfirm(null);
      await load();
    } catch (raw: unknown) {
      setActionError(toAppError(raw));
    } finally {
      setBusy(null);
    }
  }, [api, confirm, load, t]);

  const cancelRestore = useCallback(async () => {
    setBusy("cancel");
    setActionError(null);
    setMessage(null);
    try {
      await api.profileCancelRestore();
      setMessage(t("dataRestoreCancelDone"));
      await load();
    } catch (raw: unknown) {
      setActionError(toAppError(raw));
    } finally {
      setBusy(null);
    }
  }, [api, load, t]);

  return (
    <div className="flex min-h-0 flex-1 flex-col" data-testid="data-view">
      <div className="flex flex-wrap items-center gap-2 border-b border-separator-border px-5 py-2">
        <h2 className="text-sm font-semibold">{t("dataTitle")}</h2>
        <span className="text-[11px] text-text-tertiary">{t("dataHint")}</span>
        <button
          type="button"
          className="ml-auto rounded-md border border-separator-border px-3 py-1 text-[11px] text-text-secondary hover:bg-background-primary-hover disabled:opacity-60"
          onClick={() => void load()}
          disabled={status === "loading" || busy !== null}
          data-testid="data-refresh"
        >
          {t("dataRefresh")}
        </button>
        <button
          type="button"
          className="rounded-md border border-separator-border bg-background-primary-default px-3 py-1 text-[11px] hover:bg-background-primary-hover disabled:opacity-60"
          onClick={() => void backup()}
          disabled={busy !== null || status !== "ready" || data === null}
          data-testid="data-backup"
        >
          {busy === "backup" ? t("dataBackingUp") : t("dataBackupNow")}
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-auto px-5 pb-8">
        {message ? (
          <p className="mt-3 text-[11px] text-state-success-text" data-testid="data-message">
            {message}
          </p>
        ) : null}
        {actionError ? (
          <div className="mt-3">
            <ErrorPanel error={actionError} scope="data-action" compact />
          </div>
        ) : null}
        {status === "loading" ? <LoadingPanel label={t("loading")} testId="data-loading" /> : null}
        {status === "error" && error && data === null ? (
          <ErrorPanel error={error} onRetry={() => void load()} scope="data" />
        ) : null}
        {status === "error" && error && data !== null ? (
          <div className="mt-3">
            <ErrorPanel error={error} onRetry={() => void load()} scope="data-refresh" compact />
          </div>
        ) : null}

        {data ? (
          <>
            <section className="mt-3">
              <h3 className="text-xs font-semibold text-text-primary">{t("dataProfileTitle")}</h3>
              <dl className="mt-2 grid grid-cols-[10rem_1fr] gap-x-3 gap-y-1 text-[11px]">
                <dt className="text-text-tertiary">{t("dataProfile")}</dt>
                <dd data-testid="data-profile">{data.profile}</dd>
                <dt className="text-text-tertiary">{t("dataPath")}</dt>
                <dd className="break-all" data-testid="data-root">
                  {data.dataRoot}
                </dd>
                <dt className="text-text-tertiary">{t("dataDatabase")}</dt>
                <dd className="break-all" data-testid="data-database">
                  {data.databaseFile}
                </dd>
                <dt className="text-text-tertiary">{t("dataSize")}</dt>
                <dd data-testid="data-size">
                  {bytesText(data.databaseBytes)} · {t("dataSchemaValue", {
                    actual: data.databaseSchema,
                    expected: data.expectedSchema,
                  })}{" "}
                  · {data.journalMode}
                </dd>
                <dt className="text-text-tertiary">{t("dataBackupDir")}</dt>
                <dd className="break-all">{data.backupDirectory}</dd>
              </dl>
              <p className="mt-2 text-[10px] leading-4 text-text-tertiary" data-testid="data-scope">
                {t("dataScopeNote")}
              </p>
            </section>

            {data.pendingRestore ? (
              <section
                className="mt-4 rounded-md border border-border-warning-default/40 bg-background-tertiary-warning px-3 py-2 text-[11px] text-state-warning-text"
                data-testid="data-pending-restore"
              >
                <p className="font-medium">
                  {t("dataRestorePending", { file: data.pendingRestore.backupFileName })}
                </p>
                <p className="mt-1 text-[10px] leading-4 text-text-secondary">
                  {t(
                    data.pendingRestore.failed
                      ? "dataRestoreFailedHint"
                      : "dataRestorePendingHint",
                  )}
                </p>
                <button
                  type="button"
                  className="mt-1 rounded-md border border-separator-border px-3 py-0.5 text-[10px] text-text-secondary hover:bg-background-primary-hover disabled:opacity-60"
                  onClick={() => void cancelRestore()}
                  disabled={busy !== null}
                  data-testid="data-restore-cancel"
                >
                  {t("dataRestoreCancel")}
                </button>
              </section>
            ) : null}

            {data.lastRestore ? (
              <section className="mt-4 text-[10px] text-text-tertiary" data-testid="data-last-restore">
                <h3 className="text-xs font-semibold text-text-primary">{t("dataRestoreLast")}</h3>
                <p className="mt-1">
                  {t("dataRestoreLastLine", {
                    file: data.lastRestore.restoredFrom,
                    at: localTime(data.lastRestore.appliedAt),
                  })}
                </p>
                {data.lastRestore.previousCopy ? (
                  <p className="mt-1 break-all">
                    {t("dataRestorePreviousCopy")}: {data.lastRestore.previousCopy}
                  </p>
                ) : null}
              </section>
            ) : null}

            <section className="mt-4">
              <h3 className="text-xs font-semibold text-text-primary">{t("dataBackupsTitle")}</h3>
              {data.backups.length === 0 ? (
                <p className="mt-1 text-[11px] text-text-tertiary" data-testid="data-backups-empty">
                  {t("dataBackupEmpty")}
                </p>
              ) : (
                <div className="mt-2 overflow-x-auto">
                  <table className="w-full border-collapse text-left text-[11px]">
                    <thead>
                      <tr className="text-[10px] uppercase tracking-wide text-text-tertiary">
                        <th className="py-1 pr-2 font-medium">{t("dataBackupFile")}</th>
                        <th className="py-1 pr-2 font-medium">{t("dataBackupCreated")}</th>
                        <th className="py-1 pr-2 font-medium">{t("dataBackupSize")}</th>
                        <th className="py-1 pr-2 font-medium">{t("dataBackupSchema")}</th>
                        <th className="py-1 pr-2 font-medium">{t("dataBackupIntegrity")}</th>
                        <th className="py-1 font-medium">{t("dataBackupActions")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {data.backups.map((record) => (
                        <tr
                          key={record.path}
                          className="border-t border-separator-border"
                          data-testid={`data-backup-${record.manifest.fileName}`}
                        >
                          <td className="py-1 pr-2 font-mono text-[10px] text-text-primary">
                            {record.manifest.fileName}
                          </td>
                          <td className="py-1 pr-2 text-text-secondary">
                            {localTime(record.manifest.createdAt)}
                          </td>
                          <td className="py-1 pr-2 text-text-secondary">
                            {bytesText(record.manifest.sizeBytes)}
                          </td>
                          <td className="py-1 pr-2 text-text-secondary">
                            {record.manifest.databaseSchema}
                          </td>
                          <td className="py-1 pr-2">
                            <Badge tone={integrityTone(record.integrity)}>
                              {t(`dataIntegrity.${record.integrity}`, {
                                defaultValue: record.integrity,
                              })}
                            </Badge>
                          </td>
                          <td className="py-1">
                            <button
                              type="button"
                              className="rounded-md border border-separator-border px-2 py-0.5 text-[10px] text-text-secondary hover:bg-background-primary-hover disabled:opacity-50"
                              onClick={() => setConfirm(record)}
                              disabled={record.integrity !== "ok" || busy !== null}
                              data-testid={`data-restore-${record.manifest.fileName}`}
                            >
                              {t("dataRestore")}
                            </button>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
              <p className="mt-2 text-[10px] leading-4 text-text-tertiary">
                {t("dataRestoreManualHint")}
              </p>
            </section>
          </>
        ) : null}
      </div>

      {confirm ? (
        <div
          className="fixed inset-0 z-40 flex items-center justify-center bg-black/40 p-6"
          role="dialog"
          aria-modal="true"
          aria-labelledby="data-restore-confirm-title"
        >
          <div
            className="w-full max-w-lg rounded-lg border border-separator-border bg-background-primary-default p-4 shadow-lg"
            data-testid="data-restore-confirm"
          >
            <h2 id="data-restore-confirm-title" className="text-sm font-semibold">
              {t("dataRestoreConfirmTitle")}
            </h2>
            <p className="mt-2 text-xs text-text-secondary">
              {t("dataRestoreConfirmBody", {
                file: confirm.manifest.fileName,
                at: localTime(confirm.manifest.createdAt),
              })}
            </p>
            {actionError ? (
              <div className="mt-3">
                <ErrorPanel error={actionError} scope="data-restore-confirm" compact />
              </div>
            ) : null}
            <div className="mt-4 flex justify-end gap-2">
              <button
                type="button"
                className="rounded-md border border-separator-border px-3 py-1.5 text-xs disabled:opacity-60"
                onClick={() => setConfirm(null)}
                disabled={busy === "restore"}
                autoFocus
                data-testid="data-restore-confirm-cancel"
              >
                {t("quitCancel")}
              </button>
              <button
                type="button"
                className="rounded-md bg-background-primary-default px-3 py-1.5 text-xs shadow-sm ring-1 ring-separator-border disabled:opacity-60"
                onClick={() => void planRestore()}
                disabled={busy !== null}
                data-testid="data-restore-confirm-ok"
              >
                {t("dataRestoreConfirmOk")}
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
