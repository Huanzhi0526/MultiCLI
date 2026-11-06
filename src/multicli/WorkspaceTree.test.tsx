import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import "@/i18n/multicli";
import WorkspaceTree from "./WorkspaceTree";
import type { WorkspaceListItem } from "./types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let host: HTMLDivElement;
afterEach(async () => { await act(async () => root?.unmount()); host?.remove(); root = null; });
const item = (id: string, path: string): WorkspaceListItem => ({ workspace: {
  id, path: { hostId: "local", displayPath: path, canonicalPath: path }, repoGroupId: null, worktreeId: null,
}, sessionCount: 2 });

it("opens one folder level at a time without changing the session filter, and keeps expansion when another page arrives", async () => {
  const selected = vi.fn();
  function Harness({ items }: { items: WorkspaceListItem[] }) {
    const [expanded, setExpanded] = useState<Set<string>>(new Set());
    return <WorkspaceTree items={items} expanded={expanded} selectedId="project" onSelect={selected}
      onToggle={key => setExpanded(previous => { const next = new Set(previous); if (next.has(key)) next.delete(key); else next.add(key); return next; })} />;
  }
  host = document.createElement("div"); document.body.append(host); root = createRoot(host);
  const initial = [item("parent", "D:\\Work"), item("project", "D:\\Work\\项目")];
  const render = async (items: WorkspaceListItem[]) => { await act(async () => root!.render(<Harness items={items} />)); };
  await render(initial);
  expect(host.textContent).toBe("D:\\");
  const toggle = () => host.querySelector<HTMLButtonElement>('[data-testid="workspace-folder-toggle"]')!;
  await act(async () => toggle().click());
  expect(host.querySelector('[data-testid="workspace-row-parent"]')).not.toBeNull();
  expect(host.querySelector('[data-testid="workspace-row-project"]')).toBeNull();
  expect(selected).not.toHaveBeenCalled();
  const nestedToggle = host.querySelectorAll<HTMLButtonElement>('[data-testid="workspace-folder-toggle"]')[1];
  await act(async () => nestedToggle.click());
  const project = host.querySelector<HTMLButtonElement>('[data-testid="workspace-row-project"]')!;
  expect(project.textContent).toBe("项目2");
  expect(project.title).toContain("D:\\Work\\项目");
  expect(project.getAttribute("aria-current")).toBe("true");
  await act(async () => project.click());
  expect(selected).toHaveBeenCalledExactlyOnceWith("project");
  await render([...initial, item("other", "D:\\Work\\Other")]);
  expect(host.querySelector('[data-testid="workspace-row-other"]')).not.toBeNull();
  await act(async () => toggle().click());
  expect(host.querySelector('[data-testid="workspace-row-project"]')).toBeNull();
  expect(selected).toHaveBeenCalledTimes(1);
  await act(async () => toggle().click());
  expect(host.querySelector('[data-testid="workspace-row-project"]')).not.toBeNull();
});
