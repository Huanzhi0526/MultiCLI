/**
 * Pure navigation helpers: request sequencing, cursor merge and presentation
 * rules that must hold regardless of which entry (workspace / agent) opened a
 * session.
 */

import type {
  AppError,
  Capability,
  ErrorCode,
  Message,
  Page,
  SessionOrigin,
} from "./types";

const ERROR_CODES: ReadonlySet<string> = new Set<ErrorCode>([
  "unsupported",
  "unverified",
  "invalid_argument",
  "not_found",
  "access_denied",
  "conflict",
  "timeout",
  "cancelled",
  "io",
  "database",
  "protocol",
]);

/** Requests are tagged so a late response cannot overwrite a newer view. */
export class RequestGate {
  private token = 0;

  begin(): number {
    this.token += 1;
    return this.token;
  }

  isCurrent(token: number): boolean {
    return token === this.token;
  }

  invalidate(): void {
    this.token += 1;
  }
}

/** Appends a page without duplicates or reordering already shown rows. */
export function mergePageItems<T>(
  existing: readonly T[],
  incoming: readonly T[],
  idOf: (item: T) => string,
): T[] {
  const seen = new Set(existing.map(idOf));
  const merged = [...existing];
  for (const item of incoming) {
    const id = idOf(item);
    if (!seen.has(id)) {
      seen.add(id);
      merged.push(item);
    }
  }
  return merged;
}

export function sessionFilterKey(
  entry: "workspaces" | "agents",
  workspaceId: string | null,
  agentId: string | null,
  origin: SessionOrigin | null,
): string {
  const scope = entry === "workspaces" ? workspaceId : agentId;
  return JSON.stringify(["sessions", entry, scope, origin]);
}

export function isNotFound(error: AppError | null): boolean {
  return error?.code === "not_found";
}

/** Normalizes anything Tauri rejects with into the frozen AppError shape. */
export function toAppError(raw: unknown): AppError {
  if (raw && typeof raw === "object") {
    const candidate = raw as Record<string, unknown>;
    const code = typeof candidate.code === "string" && ERROR_CODES.has(candidate.code)
      ? (candidate.code as ErrorCode)
      : "protocol";
    const message =
      typeof candidate.message === "string" && candidate.message.length > 0
        ? candidate.message
        : "The operation failed";
    return {
      code,
      message,
      retryable: candidate.retryable === true,
      details:
        candidate.details && typeof candidate.details === "object" && !Array.isArray(candidate.details)
          ? (candidate.details as Record<string, unknown>)
          : {},
    };
  }
  return {
    code: "protocol",
    message: typeof raw === "string" && raw.length > 0 ? raw : String(raw ?? "Unknown failure"),
    retryable: false,
    details: {},
  };
}

export interface CapabilitySummary {
  supported: number;
  unsupported: number;
  unverified: number;
  document: number;
  fixture: number;
  live: number;
}

/** Status and evidence are counted on separate axes on purpose. */
export function summarizeCapabilities(items: readonly Capability[]): CapabilitySummary {
  const summary: CapabilitySummary = {
    supported: 0,
    unsupported: 0,
    unverified: 0,
    document: 0,
    fixture: 0,
    live: 0,
  };
  for (const item of items) {
    summary[item.status] += 1;
    summary[item.evidenceLevel] += 1;
  }
  return summary;
}

/** Extracts readable text from a frozen Message.content object. */
export function messageText(message: Pick<Message, "content">): string {
  const content = message.content;
  for (const key of ["text", "content", "message"]) {
    const value = content[key];
    if (typeof value === "string" && value.trim().length > 0) {
      return truncate(value);
    }
  }
  try {
    return truncate(JSON.stringify(content));
  } catch {
    return "";
  }
}

function truncate(value: string, limit = 4000): string {
  return value.length > limit ? `${value.slice(0, limit)}…` : value;
}

/** Empty pages are only legitimate when the backend actually returned one. */
export function emptyPage<T>(total = 0): Page<T> {
  return { items: [], nextCursor: null, total };
}

export function pageItemCount(page: Page<unknown> | null): number {
  return page?.items.length ?? 0;
}
