import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { MultiCliApi } from "../api";
import { Badge, EmptyPanel, ErrorPanel, LoadingPanel } from "../components";
import { toAppError } from "../navigation";
import type {
  AppError,
  Message,
  ResumeTargetView,
  SessionMetadataView,
} from "../types";
import type { HistorySessionState } from "./useHistory";

function resumeTone(status: ResumeTargetView["status"]): "green" | "red" | "amber" | "neutral" {
  if (status === "resolved") return "green";
  if (status === "blocked") return "red";
  if (status === "unknown") return "amber";
  return "neutral";
}

/**
 * Readable body without dumping the whole content object: unknown field names
 * are allowed to reach the UI, their values are not, and a tool message shows
 * its input instead of a JSON blob of metadata.
 */
function messageBody(message: Message): string {
  const text = message.content["text"];
  if (typeof text === "string" && text.trim().length > 0) {
    return text;
  }
  const tool = message.content["tool"];
  if (tool && typeof tool === "object" && !Array.isArray(tool)) {
    const input = (tool as Record<string, unknown>)["input"];
    if (typeof input === "string") {
      return input;
    }
  }
  return "";
}

function MessageRow({
  message,
  anchored,
  registerAnchor,
}: {
  message: Message;
  anchored: boolean;
  registerAnchor: (node: HTMLLIElement | null) => void;
}) {
  const { t } = useTranslation("multicli");
  const body = messageBody(message);
  const tool = message.content["tool"];
  const toolName =
    tool && typeof tool === "object" && !Array.isArray(tool)
      ? (tool as Record<string, unknown>)["name"]
      : null;
  const toolInput =
    tool && typeof tool === "object" && !Array.isArray(tool)
      ? (tool as Record<string, unknown>)["input"]
      : null;
  const unknown = message.content["unknownFields"];
  const unknownFields = Array.isArray(unknown)
    ? unknown.filter((item): item is string => typeof item === "string")
    : [];
  const truncated = message.content["truncated"] === true;
  const long = body.length > 2000;

  return (
    <li
      id={`history-message-${message.id}`}
      ref={anchored ? registerAnchor : undefined}
      className={
        anchored
          ? "rounded-lg border border-accent-300 bg-accent-50 px-3 py-2"
          : "rounded-lg border border-separator-border px-3 py-2"
      }
      data-testid={`history-message-${message.id}`}
      data-anchored={anchored ? "true" : "false"}
    >
      <div className="flex flex-wrap items-center gap-2 text-[10px] text-text-tertiary">
        <span>{t("messageSeq", { seq: message.seq })}</span>
        <code>{message.kind}</code>
        <span>{message.observedAt}</span>
        {typeof toolName === "string" ? <code>{toolName}</code> : null}
        {truncated ? <Badge tone="amber">{t("historySnippetTruncated")}</Badge> : null}
      </div>
      {long ? (
        <details className="mt-1">
          <summary className="cursor-pointer text-[11px] text-text-tertiary">
            {body.slice(0, 240)}…
          </summary>
          <p className="mt-1 whitespace-pre-wrap break-words text-xs leading-5 text-text-primary">
            {body}
          </p>
        </details>
      ) : (
        <p className="mt-1 whitespace-pre-wrap break-words text-xs leading-5 text-text-primary">
          {body || `[${message.kind}]`}
        </p>
      )}
      {toolInput ? (
        <details className="mt-1 text-[10px] text-text-tertiary">
          <summary className="cursor-pointer">{t("historyToolInput")}</summary>
          <pre className="mt-1 max-h-64 overflow-auto whitespace-pre-wrap break-all rounded bg-background-secondary-default p-2">
            {typeof toolInput === "string" ? toolInput : JSON.stringify(toolInput, null, 2)}
          </pre>
        </details>
      ) : null}
      {unknownFields.length > 0 ? (
        <details className="mt-1 text-[10px] text-text-tertiary">
          <summary className="cursor-pointer">{t("historyUnknownFields")}</summary>
          <p className="mt-1 break-all font-mono">{unknownFields.join(", ")}</p>
        </details>
      ) : null}
      {message.sourceRef ? (
        <p className="mt-1 text-[9px] text-text-tertiary">
          <code>{message.sourceRef}</code>
        </p>
      ) : null}
    </li>
  );
}

function Metadata({ metadata }: { metadata: SessionMetadataView }) {
  const { t } = useTranslation("multicli");
  const session = metadata.session;
  return (
    <div className="border-b border-separator-border px-5 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="text-base font-semibold" data-testid="history-detail-title">
          {session.title || t("untitledSession")}
        </h2>
        <Badge tone="neutral">{t("historyReadOnly")}</Badge>
        <Badge tone="blue">{t(`origin.${session.origin}`)}</Badge>
      </div>
      <p className="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-text-tertiary">
        <span>
          <code data-testid="history-detail-session-id">{session.id}</code>
        </span>
        <span>
          {t("agent")}: <code>{session.agentId}</code>
        </span>
        <span>
          {t("nativeId")}: <code>{session.nativeId ?? t("historyNotAvailable")}</code>
        </span>
      </p>
      <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-[11px] sm:grid-cols-4">
        <div>
          <dt className="text-[10px] text-text-tertiary">{t("messageCountLabel")}</dt>
          <dd data-testid="history-detail-count">{metadata.messageCount}</dd>
        </div>
        <div>
          <dt className="text-[10px] text-text-tertiary">{t("runCountLabel")}</dt>
          <dd>{metadata.runTotal}</dd>
        </div>
        <div>
          <dt className="text-[10px] text-text-tertiary">{t("historyFirstMessage")}</dt>
          <dd className="truncate">{metadata.firstMessageAt ?? t("historyNotAvailable")}</dd>
        </div>
        <div>
          <dt className="text-[10px] text-text-tertiary">{t("historyLastMessage")}</dt>
          <dd className="truncate">{metadata.lastMessageAt ?? t("historyNotAvailable")}</dd>
        </div>
      </dl>
      {metadata.history ? (
        <div className="mt-2 flex flex-wrap items-center gap-1.5 text-[10px] text-text-tertiary">
          <span>{t("historyProvenance")}:</span>
          {metadata.history.sourceIds.map((id) => (
            <code key={id}>{id}</code>
          ))}
          <span>· {t("historyFiles", {
            done: metadata.history.fileCount,
            total: metadata.history.fileCount,
            skipped: 0,
            failed: 0,
          })}</span>
          <span>
            · {t("historyParserVersion", { version: metadata.history.parserVersions.join(", v") })}
          </span>
        </div>
      ) : null}
      {metadata.workspaces.length > 0 ? (
        <ul className="mt-2 flex flex-col gap-1">
          {metadata.workspaces.map((workspace) => (
            <li
              key={workspace.workspaceId}
              className="flex flex-wrap items-center gap-1.5 text-[10px] text-text-tertiary"
            >
              <Badge tone={workspace.role === "primary" ? "blue" : "neutral"}>
                {workspace.role === "primary" ? t("primary") : t("related")}
              </Badge>
              <span className="break-all">
                {workspace.path?.displayPath ?? workspace.workspaceId}
              </span>
              <span>· {workspace.evidence}</span>
            </li>
          ))}
        </ul>
      ) : null}
      {metadata.messageKinds.length > 0 ? (
        <p className="mt-2 flex flex-wrap gap-1.5 text-[10px] text-text-tertiary">
          <span>{t("historyEvents")}:</span>
          {metadata.messageKinds.map((kind) => (
            <span key={kind.kind}>
              <code>{kind.kind}</code> {kind.count}
            </span>
          ))}
        </p>
      ) : null}
    </div>
  );
}

export function ResumeBlock({ api, sessionId, onResume }: { api: MultiCliApi; sessionId: string; onResume?: (sessionId: string) => void }) {
  const { t } = useTranslation("multicli");
  const [target, setTarget] = useState<ResumeTargetView | null>(null);
  const [error, setError] = useState<AppError | null>(null);
  const [checking, setChecking] = useState(false);
  const requestRef = useRef(0);

  useEffect(() => {
    // Invalidate any in-flight check: its answer belongs to the old session.
    requestRef.current += 1;
    setTarget(null);
    setError(null);
    setChecking(false);
    return () => { requestRef.current += 1; };
  }, [sessionId]);

  const check = useCallback(() => {
    const request = ++requestRef.current;
    setChecking(true);
    setError(null);
    void api
      .resolveResumeTarget(sessionId)
      .then((next) => {
        if (requestRef.current === request) {
          setTarget(next);
        }
      })
      .catch((raw: unknown) => {
        if (requestRef.current === request) {
          setError(toAppError(raw));
        }
      })
      .finally(() => {
        if (requestRef.current === request) {
          setChecking(false);
        }
      });
  }, [api, sessionId]);

  const canResume = Boolean(onResume && target?.sessionId === sessionId && target.status === "resolved" && target.executionAvailable);

  return (
    <section className="border-b border-separator-border px-5 py-3" data-testid="history-resume">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-text-secondary">
          {t("historyResumeCheck")}
        </h3>
        <button
          type="button"
          className="rounded-md border border-separator-border px-2 py-1 text-[11px] hover:bg-background-primary-hover disabled:opacity-50"
          onClick={check}
          disabled={checking}
          data-testid="history-resume-check"
        >
          {checking ? t("historyResumeChecking") : t("historyResumeCheck")}
        </button>
        <button
          type="button"
          className="ml-auto rounded-md border border-separator-border px-2 py-1 text-[11px] disabled:opacity-60"
          disabled={!canResume || checking}
          onClick={() => { if (canResume) onResume?.(sessionId); }}
          title={t("historyResumeDisabled")}
          data-testid="history-resume-disabled"
        >
          {t("historyResumeNoExecution")}
        </button>
      </div>
      {error ? (
        <div className="mt-2">
          <ErrorPanel error={error} onRetry={check} scope="history-resume" compact />
        </div>
      ) : null}
      {target ? (
        <div className="mt-2 flex flex-col gap-1">
          <div className="flex flex-wrap items-center gap-1.5">
            <Badge tone={resumeTone(target.status)} testId="history-resume-status">
              {t(`historyResume.${target.status}`)}
            </Badge>
            <code className="text-[10px] text-text-tertiary">
              {target.nativeId ?? t("historyNotAvailable")}
            </code>
            {target.product ? <span className="text-[10px]">{target.product}</span> : null}
          </div>
          <p className="text-[11px] text-text-secondary">
            {t("historyResumeReason")}: {target.reason}
          </p>
          {target.conflicts.length > 0 ? (
            <ul className="flex flex-col gap-1">
              {target.conflicts.map((conflict, index) => (
                <li
                  key={`${conflict.kind}-${index}`}
                  className="rounded border border-red-200 bg-red-50 px-2 py-1 text-[10px] text-red-700"
                >
                  <code>{conflict.kind}</code> · {conflict.detail}
                </li>
              ))}
            </ul>
          ) : null}
          {target.evidence.length > 0 ? (
            <p className="text-[10px] text-text-tertiary">{target.evidence.join(" · ")}</p>
          ) : null}
          <p className="text-[10px] leading-4 text-text-tertiary">{target.note}</p>
        </div>
      ) : null}
    </section>
  );
}

export default function SessionPanel({
  api,
  state,
  onResume,
}: {
  api: MultiCliApi;
  state: HistorySessionState;
  onResume?: (sessionId: string) => void;
}) {
  const { t } = useTranslation("multicli");
  const { metadata, page, status, error, loadMore, retry } = state;
  const anchorRef = useRef<HTMLLIElement | null>(null);
  const scrolledFor = useRef<string | null>(null);

  // A search hit opens the same session and asks the backend for a page that
  // already contains the message. The highlight alone would leave the reader at
  // the top of a long transcript, so scroll once per anchor id.
  useEffect(() => {
    const anchorId = page?.anchorMessageId ?? null;
    if (!anchorId) {
      scrolledFor.current = null;
      return;
    }
    if (scrolledFor.current === anchorId) {
      return;
    }
    const node = anchorRef.current;
    if (!node) {
      return;
    }
    scrolledFor.current = anchorId;
    // jsdom and very old webviews do not implement scrolling; the highlight
    // still marks the row, so failing to scroll is not an error.
    node.scrollIntoView?.({ block: "center" });
  }, [page?.anchorMessageId, page?.items.length]);

  const registerAnchor = useCallback((node: HTMLLIElement | null) => {
    anchorRef.current = node;
  }, []);

  if (status === "idle") {
    return (
      <EmptyPanel
        title={t("historySessionEmpty")}
        hint={t("historySessionEmptyHint")}
        testId="history-detail-empty"
      />
    );
  }
  if (status === "loading" && !metadata) {
    return <LoadingPanel label={t("loading")} testId="history-detail-loading" />;
  }
  if (status === "error" && error) {
    return (
      <div className="p-5">
        <ErrorPanel error={error} onRetry={retry} scope="history-detail" />
      </div>
    );
  }
  if (!metadata || !page) {
    return (
      <EmptyPanel
        title={t("historySessionEmpty")}
        hint={t("historySessionEmptyHint")}
        testId="history-detail-empty"
      />
    );
  }

  return (
    <article className="flex min-h-0 flex-col" data-testid="history-detail">
      <Metadata metadata={metadata} />
      <ResumeBlock api={api} sessionId={metadata.session.id} onResume={onResume} />
      <section className="min-h-0 flex-1 overflow-auto px-5 py-3">
        <div className="flex items-center gap-2">
          <h3 className="text-xs font-semibold uppercase tracking-wide text-text-secondary">
            {t("historyMessages")}
          </h3>
          <span className="text-[10px] text-text-tertiary" data-testid="history-message-count">
            {t("listCount", { items: page.items.length, total: page.total })}
          </span>
          {page.anchorMessageId ? (
            <Badge tone="blue" testId="history-anchored">
              {t("historyAnchored")}
            </Badge>
          ) : null}
        </div>
        <ol className="mt-2 flex flex-col gap-2">
          {page.items.map((message) => (
            <MessageRow
              key={message.id}
              message={message}
              anchored={message.id === page.anchorMessageId}
              registerAnchor={registerAnchor}
            />
          ))}
        </ol>
        {page.items.length === 0 ? (
          <p className="mt-2 text-xs text-text-tertiary">{t("noMessages")}</p>
        ) : null}
        {status === "loading-more" ? <LoadingPanel label={t("loading")} /> : null}
        {status === "error-more" && error ? (
          <ErrorPanel error={error} onRetry={loadMore} scope="history-detail-more" compact />
        ) : null}
        {page.nextCursor ? (
          <button
            type="button"
            className="mt-2 w-full rounded-md border border-separator-border px-3 py-1.5 text-xs text-text-secondary hover:bg-background-primary-hover"
            onClick={loadMore}
            data-testid="history-messages-load-more"
          >
            {t("historyLoadMore")}
          </button>
        ) : null}
      </section>
    </article>
  );
}
