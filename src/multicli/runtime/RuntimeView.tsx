import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { MultiCliApi } from "../api";
import type {
  RuntimeCapabilityStatus,
  RuntimeCapabilityView,
  RuntimePermissionView,
  RuntimeRunView,
  RuntimeToolView,
} from "../types";
import { ErrorPanel } from "../components";
import { toAppError } from "../navigation";
import {
  authorizationLabelKey,
  authorizationTone,
  canCancel,
  canResumeAsNewRun,
  canSend,
  executionLabelKey,
  formatTimestamp,
  isRunLive,
  pendingPermissions,
  permissionHistory,
  previewText,
} from "./runtimeModel";
import { useRuntime } from "./useRuntime";

const TONE_CLASS: Record<string, string> = {
  ok: "text-state-success-text",
  warn: "text-text-secondary",
  bad: "text-text-error-primary",
  muted: "text-text-tertiary",
};

const STATUS_CLASS: Record<RuntimeCapabilityStatus, string> = {
  supported: "bg-state-success-base text-state-success-text",
  unsupported: "bg-background-tertiary-error text-text-error-primary",
  unverified: "bg-background-secondary-default text-text-secondary",
};

function CapabilityBadge({
  label,
  status,
}: {
  label: string;
  status: RuntimeCapabilityStatus;
}) {
  return (
    <span
      className={`rounded px-1.5 py-0.5 text-[10px] ${STATUS_CLASS[status] ?? STATUS_CLASS.unverified}`}
      data-testid={`runtime-capability-${label}`}
    >
      {label}:{status}
    </span>
  );
}

function CapabilityRow({ item }: { item: RuntimeCapabilityView }) {
  const { t } = useTranslation("multicli");
  return (
    <div
      className="rounded-md border border-separator-border p-2"
      data-testid={`runtime-capability-row-${item.product}`}
    >
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-xs font-medium">{item.productName}</span>
        <span className="text-[10px] text-text-tertiary">
          {item.version ?? t("runtimeVersionUnknown")} · {item.transport}
        </span>
      </div>
      <div className="mt-1 flex flex-wrap gap-1">
        <CapabilityBadge label={t("runtimeFieldChat")} status={item.structuredChat} />
        <CapabilityBadge label={t("runtimeFieldPermission")} status={item.permissionFlow} />
        <CapabilityBadge label={t("runtimeFieldTools")} status={item.toolEvents} />
        <CapabilityBadge label={t("runtimeFieldCancel")} status={item.cancel} />
        <CapabilityBadge label={t("runtimeFieldResume")} status={item.resume} />
      </div>
      <p className="mt-1 text-[10px] text-text-tertiary">
        {t("runtimeEvidence")}: {item.evidence} · {item.reason ?? ""}
      </p>
    </div>
  );
}

function ToolCard({ tool }: { tool: RuntimeToolView }) {
  const { t } = useTranslation("multicli");
  const tone = authorizationTone(tool.authorization);
  return (
    <div
      className="rounded-md border border-separator-border bg-background-secondary-default p-2"
      data-testid={`runtime-tool-${tool.toolCallId}`}
    >
      <div className="flex flex-wrap items-baseline gap-2">
        <span className="text-xs font-medium">{tool.name}</span>
        <span className="rounded bg-background-primary-default px-1 text-[10px] text-text-secondary">
          {tool.kind}
        </span>
        <span className={`text-[10px] ${TONE_CLASS[tone]}`}>
          {t(authorizationLabelKey(tool.authorization))}
        </span>
        <span className="text-[10px] text-text-secondary">
          {t(executionLabelKey(tool.execution, tool.outcome))}
        </span>
        {/* The two ids stay separate on screen: a tool call and a permission
            request are different things and folding them together would make
            a report impossible to match. */}
        <span className="ml-auto font-mono text-[9px] text-text-tertiary">
          tool:{tool.toolCallId.slice(0, 12)}
          {tool.permissionId ? ` · perm:${tool.permissionId.slice(0, 10)}` : ""}
        </span>
      </div>
      {tool.output ? (
        <pre className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-words text-[10px] text-text-secondary">
          {previewText(tool.output, 1200)}
        </pre>
      ) : null}
    </div>
  );
}

function PermissionCard({
  permission,
  busy,
  onAnswer,
}: {
  permission: RuntimePermissionView;
  busy: boolean;
  onAnswer: (optionId: string) => void;
}) {
  const { t } = useTranslation("multicli");
  const pending = permission.state === "pending";
  return (
    <div
      className={
        pending
          ? "rounded-md border border-accent-500 bg-background-secondary-default p-2"
          : "rounded-md border border-separator-border p-2"
      }
      data-testid={`runtime-permission-${permission.permissionId}`}
    >
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-xs font-medium">{permission.title}</span>
        <span className="text-[10px] text-text-tertiary">
          {t(`runtimePermissionState.${permission.state}`)}
        </span>
      </div>
      {permission.detail ? (
        <p className="mt-1 whitespace-pre-wrap break-words text-[10px] text-text-secondary">
          {previewText(permission.detail, 500)}
        </p>
      ) : null}
      <div className="mt-2 flex flex-wrap gap-1">
        {permission.options.map((option) => (
          <button
            key={option.optionId}
            type="button"
            className="rounded border border-separator-border bg-background-primary-default px-2 py-1 text-[11px] hover:bg-background-primary-hover disabled:opacity-50"
            disabled={!pending || busy}
            data-testid={`runtime-permission-${permission.permissionId}-${option.optionId}`}
            onClick={() => onAnswer(option.optionId)}
          >
            {option.label}
          </button>
        ))}
      </div>
      {permission.reason ? (
        <p className="mt-1 text-[10px] text-text-tertiary">{permission.reason}</p>
      ) : null}
      {permission.autoDenied ? (
        <p className="mt-1 text-[10px] text-text-secondary">
          {t("runtimePermissionAutoDenied")}
        </p>
      ) : null}
    </div>
  );
}

function RunHeader({
  run,
  busy,
  onCancel,
  onStop,
}: {
  run: RuntimeRunView;
  busy: boolean;
  onCancel: () => void;
  onStop: () => void;
}) {
  const { t } = useTranslation("multicli");
  return (
    <div className="border-b border-separator-border px-3 py-2" data-testid="runtime-run-header">
      <div className="flex flex-wrap items-baseline gap-2">
        <h2 className="text-sm font-semibold">{run.title}</h2>
        <span className="rounded bg-background-secondary-default px-1.5 py-0.5 text-[10px] text-text-secondary">
          {run.productName} · {run.transport}
        </span>
        <span className="text-[10px] text-text-tertiary" data-testid="runtime-execution-state">
          {t(`runtimeExecution.${run.executionState}`)}
        </span>
        <span className="text-[10px] text-text-tertiary" data-testid="runtime-interaction-state">
          {t(`runtimeInteraction.${run.interactionState}`)}
        </span>
        <div className="ml-auto flex gap-1">
          <button
            type="button"
            className="rounded border border-separator-border px-2 py-1 text-[11px] hover:bg-background-primary-hover disabled:opacity-50"
            disabled={busy || !canCancel(run)}
            onClick={onCancel}
            data-testid="runtime-cancel"
          >
            {t("runtimeCancelTurn")}
          </button>
          <button
            type="button"
            className="rounded border border-separator-border px-2 py-1 text-[11px] hover:bg-background-primary-hover disabled:opacity-50"
            disabled={busy || !isRunLive(run)}
            onClick={onStop}
            data-testid="runtime-stop"
          >
            {t("runtimeStopRun")}
          </button>
        </div>
      </div>
      <p className="mt-1 truncate font-mono text-[10px] text-text-tertiary">
        run:{run.runId} · session:{run.sessionId} · native:
        {run.nativeSessionId ?? t("runtimeNativeUnknown")} · pid:
        {run.process?.pid ?? t("runtimeNoProcess")}
      </p>
      {run.launchArgv.length > 0 ? (
        <p className="truncate font-mono text-[10px] text-text-tertiary" title={run.launchArgv.join(" ")}>
          {run.launchArgv.join(" ")}
        </p>
      ) : null}
    </div>
  );
}

function NewRunForm({
  capabilities,
  busy,
  onStart,
}: {
  capabilities: RuntimeCapabilityView[];
  busy: boolean;
  onStart: (installationId: string, cwd: string, prompt: string) => void;
}) {
  const { t } = useTranslation("multicli");
  const candidates = capabilities.filter((item) => item.installationId !== null);
  const [installationId, setInstallationId] = useState(candidates[0]?.installationId ?? "");
  const [cwd, setCwd] = useState("");
  const [prompt, setPrompt] = useState("");
  const [localError, setLocalError] = useState<string | null>(null);

  useEffect(() => {
    if (!installationId && candidates.length > 0) {
      setInstallationId(candidates[0].installationId ?? "");
    }
  }, [candidates, installationId]);

  const submit = () => {
    if (!installationId) {
      setLocalError(t("runtimeErrorNoInstallation"));
      return;
    }
    if (!cwd.trim()) {
      setLocalError(t("runtimeErrorCwdRequired"));
      return;
    }
    if (!prompt.trim()) {
      setLocalError(t("runtimeErrorPromptRequired"));
      return;
    }
    setLocalError(null);
    onStart(installationId, cwd.trim(), prompt);
  };

  return (
    <div className="rounded-md border border-separator-border p-2" data-testid="runtime-new-form">
      <p className="text-xs font-medium">{t("runtimeNewTitle")}</p>
      <label className="mt-2 block text-[10px] text-text-secondary">
        {t("runtimeTargetLabel")}
        <select
          className="mt-1 w-full rounded border border-separator-border bg-background-primary-default px-2 py-1 text-[11px]"
          value={installationId}
          data-testid="runtime-target-select"
          onChange={(event) => setInstallationId(event.target.value)}
        >
          {candidates.length === 0 ? (
            <option value="">{t("runtimeErrorNoInstallation")}</option>
          ) : null}
          {candidates.map((item) => (
            <option key={item.installationId ?? item.product} value={item.installationId ?? ""}>
              {item.productName} {item.version ?? ""}
            </option>
          ))}
        </select>
      </label>
      <label className="mt-2 block text-[10px] text-text-secondary">
        {t("runtimeCwdLabel")}
        <input
          className="mt-1 w-full rounded border border-separator-border bg-background-primary-default px-2 py-1 font-mono text-[11px]"
          value={cwd}
          placeholder="C:\\Work\\project"
          data-testid="runtime-cwd-input"
          onChange={(event) => setCwd(event.target.value)}
        />
      </label>
      <label className="mt-2 block text-[10px] text-text-secondary">
        {t("runtimePromptLabel")}
        <textarea
          className="mt-1 h-20 w-full rounded border border-separator-border bg-background-primary-default px-2 py-1 text-[11px]"
          value={prompt}
          data-testid="runtime-prompt-input"
          onChange={(event) => setPrompt(event.target.value)}
        />
      </label>
      <p className="mt-1 text-[10px] text-text-tertiary">{t("runtimeStartHint")}</p>
      {localError ? (
        <p className="mt-1 text-[10px] text-text-error-primary" data-testid="runtime-form-error">
          {localError}
        </p>
      ) : null}
      <button
        type="button"
        className="mt-2 w-full rounded bg-background-primary-default px-2 py-1 text-[11px] shadow-sm ring-1 ring-separator-border hover:bg-background-primary-hover disabled:opacity-50"
        disabled={busy || candidates.length === 0}
        onClick={submit}
        data-testid="runtime-start"
      >
        {busy ? t("runtimeStarting") : t("runtimeStart")}
      </button>
    </div>
  );
}

export default function RuntimeView({
  api,
  visible = true,
  resumeRequest,
  onResumeRequestHandled,
}: {
  api: MultiCliApi;
  visible?: boolean;
  resumeRequest?: {id: number; sessionId: string} | null;
  onResumeRequestHandled?: () => void;
}) {
  const { t } = useTranslation("multicli");
  const rt = useRuntime(api);
  const [draft, setDraft] = useState("");
  const transcriptRef = useRef<HTMLDivElement | null>(null);

  const capabilities = rt.capabilities?.items ?? [];
  const pending = useMemo(() => pendingPermissions(rt.run), [rt.run]);
  const history = useMemo(() => permissionHistory(rt.run), [rt.run]);
  const handledResume = useRef<number | null>(null);
  useEffect(() => {
    if (!resumeRequest || handledResume.current === resumeRequest.id) return;
    handledResume.current = resumeRequest.id;
    onResumeRequestHandled?.();
    // The shared hook owns pending/error/selection epochs for every entry.
    void rt.resume(resumeRequest.sessionId);
  }, [resumeRequest, onResumeRequestHandled, rt.resume]);

  // Keep the newest text in view, but never steal the position while the user
  // is reading scrollback.
  useEffect(() => {
    const node = transcriptRef.current;
    if (!node) return;
    const nearBottom = node.scrollHeight - node.scrollTop - node.clientHeight < 120;
    if (nearBottom) node.scrollTop = node.scrollHeight;
  }, [rt.run?.messages, rt.run?.tools]);

  const submitDraft = useCallback(() => {
    if (!draft.trim() || !canSend(rt.run)) return;
    const text = draft;
    setDraft("");
    void rt.send(text);
  }, [draft, rt]);

  if (!visible) {
    // Still mounted: the run keeps receiving events, and switching tabs must
    // not lose the transcript.
    return <div className="hidden" data-testid="runtime-host" />;
  }

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col" data-testid="runtime-view">
      <div className="grid shrink-0 grid-cols-3 gap-2 border-b border-separator-border p-2">
        {capabilities.length === 0 ? (
          <p className="col-span-3 text-[11px] text-text-tertiary">
            {rt.capabilitiesError
              ? t("runtimeCapabilitiesError", { message: rt.capabilitiesError.message })
              : t("runtimeCapabilitiesEmpty")}
          </p>
        ) : (
          capabilities.map((item) => (
            <CapabilityRow key={`${item.product}-${item.installationId ?? "none"}`} item={item} />
          ))
        )}
      </div>

      <div className="grid min-h-0 min-w-0 flex-1 grid-cols-[16rem_minmax(0,1fr)_20rem]">
        <aside className="flex min-h-0 flex-col gap-2 overflow-auto border-r border-separator-border p-2">
          <NewRunForm
            capabilities={capabilities}
            busy={rt.busy}
            onStart={(installationId, cwd, prompt) => void rt.start({ installationId, cwd, prompt })}
          />
          <div className="text-[10px] uppercase tracking-wide text-text-tertiary">
            {t("runtimeRuns")}
          </div>
          {rt.runs.length === 0 ? (
            <p className="text-[11px] text-text-tertiary">{t("runtimeRunsEmpty")}</p>
          ) : null}
          {rt.runs.map((item) => (
            <button
              key={item.runId}
              type="button"
              className={
                item.runId === rt.activeRunId
                  ? "rounded-md border border-accent-500 bg-background-secondary-default p-2 text-left"
                  : "rounded-md border border-separator-border p-2 text-left hover:bg-background-primary-hover"
              }
              data-testid={`runtime-run-${item.runId}`}
              onClick={() => rt.selectRun(item.runId)}
            >
              <div className="truncate text-[11px] font-medium">{item.title}</div>
              <div className="text-[10px] text-text-tertiary">
                {item.productName} · {t(`runtimeInteraction.${item.interactionState}`)}
              </div>
            </button>
          ))}
        </aside>

        <main className="flex min-h-0 min-w-0 flex-col">
          {rt.run ? (
            <>
              <RunHeader
                run={rt.run}
                busy={rt.busy}
                onCancel={() => void rt.cancel()}
                onStop={() => void rt.stop()}
              />
              <div
                ref={transcriptRef}
                className="min-h-0 flex-1 space-y-2 overflow-auto p-3"
                data-testid="runtime-transcript"
              >
                {rt.run.messages.length === 0 && rt.run.tools.length === 0 ? (
                  <p className="text-[11px] text-text-tertiary">{t("runtimeTranscriptEmpty")}</p>
                ) : null}
                {rt.run.messages.map((message) => (
                  <div
                    key={`${message.messageId}-${message.seq}`}
                    className={
                      message.role === "user"
                        ? "ml-8 rounded-md bg-background-secondary-default p-2"
                        : "mr-8 rounded-md border border-separator-border p-2"
                    }
                    data-testid={`runtime-message-${message.messageId}`}
                  >
                    <div className="mb-1 text-[10px] uppercase tracking-wide text-text-tertiary">
                      {t(`runtimeRole.${message.role}`)}
                      {message.complete ? "" : ` · ${t("runtimeStreaming")}`}
                    </div>
                    <p className="whitespace-pre-wrap break-words text-[12px]">{message.text}</p>
                  </div>
                ))}
                {rt.run.tools.map((tool) => (
                  <ToolCard key={tool.toolCallId} tool={tool} />
                ))}
                {rt.run.journalTruncated ? (
                  <p className="text-[10px] text-text-secondary">
                    {t("runtimeJournalTruncated")}
                  </p>
                ) : null}
              </div>
              <div className="border-t border-separator-border p-2">
                <div className="flex items-end gap-2">
                  <textarea
                    className="h-16 flex-1 rounded border border-separator-border bg-background-primary-default px-2 py-1 text-[12px]"
                    value={draft}
                    placeholder={t("runtimeComposerPlaceholder")}
                    data-testid="runtime-composer"
                    onChange={(event) => setDraft(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
                        event.preventDefault();
                        submitDraft();
                      }
                    }}
                  />
                  <button
                    type="button"
                    className="rounded border border-separator-border px-3 py-1 text-[11px] hover:bg-background-primary-hover disabled:opacity-50"
                    disabled={rt.busy || !canSend(rt.run) || !draft.trim()}
                    onClick={submitDraft}
                    data-testid="runtime-send"
                  >
                    {t("runtimeSend")}
                  </button>
                </div>
                <div className="mt-1 flex flex-wrap items-center gap-2 text-[10px] text-text-tertiary">
                  <span>{t("runtimeComposerHint")}</span>
                  {canResumeAsNewRun(rt.run) ? (
                    <button
                      type="button"
                      className="rounded border border-separator-border px-2 py-0.5 text-[10px] hover:bg-background-primary-hover"
                      data-testid="runtime-resume"
                      onClick={() => void rt.resume(rt.run!.sessionId)}
                    >
                      {t("runtimeResumeSameSession")}
                    </button>
                  ) : null}
                </div>
              </div>
            </>
          ) : (
            <div className="flex flex-1 items-center justify-center p-6 text-center text-[11px] text-text-tertiary">
              {t("runtimeNoRunSelected")}
            </div>
          )}
        </main>

        <aside className="flex min-h-0 flex-col gap-2 overflow-auto border-l border-separator-border p-2">
          <p className="text-[10px] uppercase tracking-wide text-text-tertiary">
            {t("runtimePermissions")}
          </p>
          {pending.length === 0 ? (
            <p className="text-[11px] text-text-tertiary">{t("runtimeNoPendingPermission")}</p>
          ) : null}
          {pending.map((permission) => (
            <PermissionCard
              key={permission.permissionId}
              permission={permission}
              busy={rt.busy}
              onAnswer={(optionId) => void rt.answer(permission.permissionId, optionId)}
            />
          ))}
          {history.filter((permission) => permission.state !== "pending").length > 0 ? (
            <>
              <p className="mt-2 text-[10px] uppercase tracking-wide text-text-tertiary">
                {t("runtimePermissionHistory")}
              </p>
              {history
                .filter((permission) => permission.state !== "pending")
                .slice(0, 8)
                .map((permission) => (
                  <PermissionCard
                    key={`history-${permission.permissionId}`}
                    permission={permission}
                    busy
                    onAnswer={() => undefined}
                  />
                ))}
            </>
          ) : null}
          {rt.run && rt.run.notes.length > 0 ? (
            <>
              <p className="mt-2 text-[10px] uppercase tracking-wide text-text-tertiary">
                {t("runtimeNotes")}
              </p>
              <ul className="space-y-1">
                {rt.run.notes.map((note, index) => (
                  <li key={`${index}-${note.slice(0, 12)}`} className="text-[10px] text-text-tertiary">
                    {note}
                  </li>
                ))}
              </ul>
            </>
          ) : null}
          {rt.run?.error ? (
            <ErrorPanel error={rt.run.error} onRetry={() => void rt.reload()} scope="runtime" compact />
          ) : null}
          {rt.run ? (
            <p className="mt-2 text-[10px] text-text-tertiary">
              {t("runtimeStartedAt")}: {formatTimestamp(rt.run.startedAt)}
            </p>
          ) : null}
        </aside>
      </div>

      {rt.error ? (
        <div className="border-t border-separator-border p-2">
          <ErrorPanel error={rt.error} onRetry={() => void rt.reload()} scope="runtime" compact />
        </div>
      ) : null}
      {rt.notice ? (
        <div className="border-t border-separator-border p-2 text-[10px] text-text-tertiary">
          {rt.notice}
        </div>
      ) : null}
    </div>
  );
}

/** Small helper so a caller can show a caught error without importing the type. */
export function describeRuntimeError(error: unknown): string {
  return toAppError(error).message;
}
