import { describe, expect, it } from "vitest";
import {
  mergePageItems,
  messageText,
  RequestGate,
  sessionFilterKey,
  summarizeCapabilities,
  toAppError,
} from "./navigation";
import type { Capability } from "./types";

describe("paged navigation helpers", () => {
  it("appends pages without duplicates or reordering", () => {
    const existing = [{ id: "a" }, { id: "b" }];
    const incoming = [{ id: "b" }, { id: "c" }];
    expect(mergePageItems(existing, incoming, (item) => item.id)).toEqual([
      { id: "a" },
      { id: "b" },
      { id: "c" },
    ]);
  });

  it("keeps an empty page empty instead of inventing rows", () => {
    expect(mergePageItems([], [], (item: { id: string }) => item.id)).toEqual([]);
  });

  it("drops late responses from a previous request", () => {
    const gate = new RequestGate();
    const first = gate.begin();
    const second = gate.begin();
    expect(gate.isCurrent(first)).toBe(false);
    expect(gate.isCurrent(second)).toBe(true);
    gate.invalidate();
    expect(gate.isCurrent(second)).toBe(false);
  });

  it("changes the session key when a filter changes", () => {
    const base = sessionFilterKey("workspaces", "w1", null, null);
    expect(base).not.toEqual(sessionFilterKey("workspaces", "w2", null, null));
    expect(base).not.toEqual(sessionFilterKey("agents", null, "a1", null));
    expect(base).not.toEqual(sessionFilterKey("workspaces", "w1", null, "managed"));
  });

  it("treats opaque scope ids as data, including wildcard and delimiter text", () => {
    expect(sessionFilterKey("workspaces", "*", null, null))
      .not.toEqual(sessionFilterKey("workspaces", null, null, null));
    expect(sessionFilterKey("workspaces", "x|managed", null, null))
      .not.toEqual(sessionFilterKey("workspaces", "x", null, "managed"));
  });

  it("normalizes Tauri rejections into the frozen AppError shape", () => {
    expect(
      toAppError({
        code: "database",
        message: "db down",
        retryable: true,
        details: { stage: "open" },
      }),
    ).toEqual({
      code: "database",
      message: "db down",
      retryable: true,
      details: { stage: "open" },
    });
    expect(toAppError("plain failure")).toEqual({
      code: "protocol",
      message: "plain failure",
      retryable: false,
      details: {},
    });
    expect(toAppError({ code: "not-a-code", message: "odd" })).toMatchObject({
      code: "protocol",
      message: "odd",
    });
  });

  it("counts capability status and evidence on separate axes", () => {
    const capability = (
      status: Capability["status"],
      evidence: Capability["evidenceLevel"],
    ): Capability => ({
      agentId: "a",
      installationId: null,
      name: "structuredChat",
      status,
      versionRange: null,
      evidenceLevel: evidence,
      source: "fixture",
      verifiedAt: null,
      reason: null,
    });
    const summary = summarizeCapabilities([
      capability("supported", "live"),
      capability("unsupported", "document"),
      capability("unverified", "fixture"),
    ]);
    expect(summary).toEqual({
      supported: 1,
      unsupported: 1,
      unverified: 1,
      document: 1,
      fixture: 1,
      live: 1,
    });
  });

  it("extracts message text without changing the stored object", () => {
    expect(messageText({ content: { text: "hello" } })).toBe("hello");
    expect(messageText({ content: { role: "assistant" } })).toContain("assistant");
    expect(messageText({ content: {} })).toBe("{}");
  });
});
