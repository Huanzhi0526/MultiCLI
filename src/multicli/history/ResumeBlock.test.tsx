import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import "@/i18n/multicli";
import { ResumeBlock } from "./SessionPanel";
import type { MultiCliApi } from "../api";
import type { ResumeTargetView } from "../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let host: HTMLDivElement;
afterEach(async () => { await act(async () => root?.unmount()); host?.remove(); root = null; });

const target = (overrides: Partial<ResumeTargetView> = {}): ResumeTargetView => ({
  sessionId: "a", agentId: "codex", product: "codex", nativeId: "native-a",
  sourceNamespace: "codex/history/local", configProfileId: null, status: "resolved",
  reason: "a native id and a product are known", reasonCode: "resolved",
  installationId: "installed", executable: null, workspaces: [], conflicts: [],
  evidence: [], executionAvailable: true, note: "Parse only", ...overrides,
});

async function mount(resolveResumeTarget: MultiCliApi["resolveResumeTarget"], onResume = vi.fn()) {
  await i18n.changeLanguage("zh");
  host = document.createElement("div"); document.body.append(host); root = createRoot(host);
  const api = { resolveResumeTarget } as MultiCliApi;
  await act(async () => root!.render(<ResumeBlock api={api} sessionId="a" onResume={onResume} />));
  return { api, onResume, button: () => host.querySelector("button")! };
}

describe("continue session from the review", () => {
  it("checks and continues exactly once from one explicit click", async () => {
    const resolve = vi.fn().mockResolvedValue(target());
    const view = await mount(resolve);
    expect(resolve).not.toHaveBeenCalled();
    expect(view.onResume).not.toHaveBeenCalled();
    await act(async () => view.button().click());
    expect(resolve).toHaveBeenCalledTimes(1);
    expect(resolve).toHaveBeenCalledWith("a");
    expect(view.onResume).toHaveBeenCalledExactlyOnceWith("a");
  });

  it("shows an actionable localized conflict and never continues a blocked target", async () => {
    const view = await mount(vi.fn().mockResolvedValue(target({ status: "blocked", reasonCode: "active-run", reason: "an active run can still write this session" })));
    await act(async () => view.button().click());
    expect(view.onResume).not.toHaveBeenCalled();
    expect(host.querySelector('[role="status"]')?.textContent).toContain("请先在终端或运行页停止它");
    expect(host.querySelector("details")?.open).toBe(false);
    expect(host.querySelector("details")?.textContent).toContain("an active run");
  });

  it("drops a late check after switching sessions", async () => {
    let finish!: (value: ResumeTargetView) => void;
    const view = await mount(vi.fn(() => new Promise<ResumeTargetView>(resolve => { finish = resolve; })));
    await act(async () => view.button().click());
    expect(view.button().disabled).toBe(true);
    await act(async () => root!.render(<ResumeBlock api={view.api} sessionId="b" onResume={view.onResume} />));
    await act(async () => finish(target()));
    expect(view.onResume).not.toHaveBeenCalled();
    expect(host.querySelector('[data-testid="history-resume-status"]')).toBeNull();
    expect(view.button().disabled).toBe(false);
  });

  it("never treats an unknown future reason code as success", async () => {
    const view = await mount(vi.fn().mockResolvedValue(target({status: "unknown", reasonCode: "future-reason"})));
    await act(async () => view.button().click());
    expect(view.onResume).not.toHaveBeenCalled();
    expect(host.querySelector('[role="status"]')?.textContent).toContain("当前无法继续");
  });
});
