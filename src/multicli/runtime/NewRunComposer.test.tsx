// @vitest-environment jsdom
import { act, StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import "@/i18n/multicli";
import NewRunComposer from "./NewRunComposer";
import type { RuntimeCapabilityView } from "../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root;
let host: HTMLDivElement;
afterEach(async () => { await act(async () => root?.unmount()); host?.remove(); });

const capability = (product: string, installationId: string | null): RuntimeCapabilityView => ({
  product, installationId, productName: product, agentId: product, version: "1.0", executable: null,
  transport: "native", structuredChat: "unverified", permissionFlow: "unsupported", toolEvents: "supported",
  cancel: "supported", resume: "unverified", evidence: "fixture", entry: null, reason: "fixture", notes: [],
});
const items = [capability("codex", "codex-install"), capability("claude-code", "claude-install"), capability("codebuddy-code", null)];
const query = <T extends HTMLElement = HTMLButtonElement>(id: string) => host.querySelector<T>(`[data-testid="${id}"]`)!;
async function mount(capabilities = items) {
  const onStart = vi.fn();
  host = document.createElement("div"); document.body.append(host); root = createRoot(host);
  await act(async () => root.render(<StrictMode><NewRunComposer capabilities={capabilities} busy={false} onStart={onStart} /></StrictMode>));
  return onStart;
}
async function type(id: string, value: string, textarea = false) {
  const element = query<HTMLInputElement | HTMLTextAreaElement>(id);
  await act(async () => {
    Object.getOwnPropertyDescriptor(textarea ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype, "value")!.set!.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function click(id: string) { await act(async () => query(id).click()); }

it("switches real product keys without starting, retains the draft, and sends to the selected installation", async () => {
  const start = await mount();
  await type("runtime-cwd-input", "D:\\工作\\项目");
  await type("runtime-prompt-input", "帮我查看项目 🧭\n保留换行", true);
  await click("cli-select-claude-code");
  expect(query("cli-select-claude-code").getAttribute("aria-pressed")).toBe("true");
  expect(query("cli-select-codex").textContent).toBe("");
  expect(query("cli-select-claude-code").textContent).toBe("Claude Code");
  expect(start).not.toHaveBeenCalled();
  expect(query<HTMLTextAreaElement>("runtime-prompt-input").value).toBe("帮我查看项目 🧭\n保留换行");
  await click("runtime-start");
  expect(start).toHaveBeenCalledExactlyOnceWith({ installationId: "claude-install", cwd: "D:\\工作\\项目", prompt: "帮我查看项目 🧭\n保留换行" });
});

it("allows inspecting a missing CLI but disables launching it", async () => {
  const start = await mount();
  await type("runtime-cwd-input", "D:\\Work"); await type("runtime-prompt-input", "hello", true);
  await click("cli-select-codebuddy-code");
  expect(query("runtime-start").disabled).toBe(true);
  await click("runtime-start");
  expect(start).not.toHaveBeenCalled();
  await click("cli-select-codex");
  expect(query("runtime-start").disabled).toBe(false);
});

it("uses an installed target on initial load without inventing support", async () => {
  await mount([capability("codex", null), capability("claude-code", "c")]);
  expect(query("cli-select-claude-code").getAttribute("aria-pressed")).toBe("true");
  expect(host.textContent).toContain("fixture");
  expect(host.textContent).toContain("未验证");
  expect(query("runtime-start").disabled).toBe(true);
});

it("does not send an incomplete draft with the keyboard shortcut", async () => {
  const start = await mount();
  await type("runtime-prompt-input", "hello", true);
  await act(async () => query("runtime-prompt-input").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true })));
  expect(start).not.toHaveBeenCalled();
  expect(query("runtime-form-error").getAttribute("role")).toBe("alert");
});
