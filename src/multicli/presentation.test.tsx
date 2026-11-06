import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import "@/i18n/multicli";
import { PathInput } from "./presentation";

const picker = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: picker }));
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let host: HTMLDivElement;
afterEach(async () => { await act(async () => root?.unmount()); root = null; host?.remove(); picker.mockReset(); });

async function mount(directory = true) {
  const changed = vi.fn();
  host = document.createElement("div"); document.body.append(host); root = createRoot(host);
  await act(async () => root!.render(<PathInput value={"D:\\Original"} onChange={changed} directory={directory} />));
  return { changed, browse: async () => { await act(async () => host.querySelector("button")!.click()); } };
}

describe("native path selection", () => {
  it("preserves the input when the user cancels", async () => {
    picker.mockResolvedValue(null);
    const view = await mount(); await view.browse();
    expect(view.changed).not.toHaveBeenCalled();
    expect(host.querySelector("input")?.value).toBe("D:\\Original");
    expect(picker).toHaveBeenCalledWith(expect.objectContaining({ directory: true, multiple: false }));
  });

  it("returns a selected executable path with spaces and Unicode literally", async () => {
    const path = "D:\\工具 😀\\my tool.exe";
    picker.mockResolvedValue(path);
    const view = await mount(false); await view.browse();
    expect(view.changed).toHaveBeenCalledExactlyOnceWith(path);
    expect(picker).toHaveBeenCalledWith(expect.objectContaining({ directory: false }));
  });

  it("surfaces a real picker failure and leaves manual input usable", async () => {
    picker.mockRejectedValue(new Error("picker unavailable"));
    const view = await mount(); await view.browse();
    expect(host.querySelector('[role="alert"]')?.textContent).toContain("picker unavailable");
    expect(host.querySelector("input")?.disabled).toBe(false);
    expect(view.changed).not.toHaveBeenCalled();
  });
});
