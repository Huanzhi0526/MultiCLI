/**
 * Pure helpers for the structured runtime view.
 *
 * The UI never derives product policy here: it renders what the backend
 * reported. The one thing it does compute is grouping — a transcript run and a
 * tool card list are both derived views of the same run, and the mapping has to
 * stay stable so a re-render cannot reorder live text.
 */

import type {
  RuntimePermissionView,
  RuntimeRunView,
  RuntimeToolView,
} from "../types";

/** A tool card is "open" while the CLI has not reported a terminal state. */
export function isToolOpen(tool: RuntimeToolView): boolean {
  return tool.execution === "running" || tool.status === "running";
}

/**
 * The authorization label. `not-requested` is its own state: Claude Code runs
 * reads and project-local edits without asking, and showing that as "denied"
 * would be a lie in the opposite direction.
 */
export function authorizationLabelKey(authorization: string): string {
  switch (authorization) {
    case "pending":
      return "runtimeAuthPending";
    case "allowed":
      return "runtimeAuthAllowed";
    case "allowed-always":
      return "runtimeAuthAllowedAlways";
    case "denied":
      return "runtimeAuthDenied";
    case "auto-denied":
      return "runtimeAuthAutoDenied";
    case "not-requested":
      return "runtimeAuthNotRequested";
    default:
      return "runtimeAuthUnknown";
  }
}

export function authorizationTone(authorization: string): "ok" | "warn" | "bad" | "muted" {
  switch (authorization) {
    case "allowed":
    case "allowed-always":
      return "ok";
    case "pending":
      return "warn";
    case "denied":
    case "auto-denied":
      return "bad";
    default:
      return "muted";
  }
}

export function executionLabelKey(execution: string, outcome: string): string {
  if (execution === "executed") {
    if (outcome === "ok") return "runtimeExecutedOk";
    if (outcome === "failed") return "runtimeExecutedFailed";
    return "runtimeExecutedUnknown";
  }
  if (execution === "not-executed") return "runtimeNotExecuted";
  if (execution === "running") return "runtimeRunning";
  return "runtimeUnknown";
}

/** Pending permission requests, oldest first. */
export function pendingPermissions(run: RuntimeRunView | null): RuntimePermissionView[] {
  if (!run) return [];
  return run.permissions.filter((permission) => permission.state === "pending");
}

/** All permissions, newest first, for the history list. */
export function permissionHistory(run: RuntimeRunView | null): RuntimePermissionView[] {
  if (!run) return [];
  return [...run.permissions].reverse();
}

export function isRunLive(run: RuntimeRunView | null): boolean {
  if (!run) return false;
  return run.executionState !== "exited" && run.executionState !== "failed";
}

/** A run can accept a new message only when no turn and no request is open. */
export function canSend(run: RuntimeRunView | null): boolean {
  if (!run) return false;
  if (!isRunLive(run)) return false;
  if (run.interactionState === "awaiting-permission") return false;
  if (run.interactionState === "awaiting-input") return false;
  return run.turn === null || run.turn.state !== "running";
}

export function canCancel(run: RuntimeRunView | null): boolean {
  if (!run) return false;
  return isRunLive(run) && run.turn !== null && run.turn.state === "running";
}

export function canResumeAsNewRun(run: RuntimeRunView | null): boolean {
  if (!run) return false;
  return run.resumable && run.nativeSessionId !== null;
}

/**
 * Trims a tool output preview without cutting a surrogate pair in half, which
 * is what makes emoji survive the fold.
 */
export function previewText(text: string | null, limit = 400): string {
  if (!text) return "";
  const chars = Array.from(text);
  if (chars.length <= limit) return text;
  return `${chars.slice(0, limit).join("")}…`;
}

export function formatTimestamp(value: string | null): string {
  if (!value) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}
