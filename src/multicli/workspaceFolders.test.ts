import { describe, expect, it } from "vitest";
import { buildWorkspaceTree } from "./workspaceFolders";
import type { WorkspaceListItem } from "./types";

export function workspaceItem(id: string, path: string, hostId = "local-windows"): WorkspaceListItem {
  return { workspace: { id, path: { hostId, displayPath: path, canonicalPath: path }, repoGroupId: null, worktreeId: null }, sessionCount: 2 };
}

describe("workspace folder hierarchy", () => {
  it("groups common ancestors while keeping parent and nested workspaces selectable", () => {
    const roots = buildWorkspaceTree([
      workspaceItem("parent", "D:\\Work"),
      workspaceItem("child", "D:\\Work\\项目 😀"),
      workspaceItem("sibling", "D:\\Work\\Other"),
    ]);
    expect(roots.map(root => root.name)).toEqual(["D:\\"]);
    const work = roots[0].children[0];
    expect(work.name).toBe("Work");
    expect(work.workspaces.map(item => item.workspace.id)).toEqual(["parent"]);
    expect(work.children).toHaveLength(2);
    expect(work.children.find(node => node.name === "项目 😀")?.workspaces[0].workspace.id).toBe("child");
  });

  it("keeps identical folder names on different drives, shares and hosts separate", () => {
    const roots = buildWorkspaceTree([
      workspaceItem("c", "C:\\Work\\Project"), workspaceItem("d", "D:\\Work\\Project"),
      workspaceItem("share1", "\\\\server\\share1\\Work"), workspaceItem("share2", "\\\\server\\share2\\Work"),
      workspaceItem("remote", "C:\\Work\\Project", "other-host"),
    ]);
    expect(roots).toHaveLength(5);
    expect(new Set(roots.map(node => node.key)).size).toBe(5);
    expect(roots.filter(node => node.name.startsWith("\\\\")).map(node => node.path).sort()).toEqual(["\\\\server\\share1", "\\\\server\\share2"]);
  });

  it("handles Windows separators, casing, extended prefixes and root workspaces", () => {
    const roots = buildWorkspaceTree([
      workspaceItem("a", "D:/Work/Project/"), workspaceItem("b", "d:\\work\\Other"),
      workspaceItem("root", "D:\\"), workspaceItem("extended", "\\\\?\\D:\\Work\\Nested"),
      workspaceItem("unc", "\\\\?\\UNC\\server\\share\\Child"),
    ]);
    const drive = roots.find(node => node.name === "D:\\")!;
    expect(drive.workspaces[0].workspace.id).toBe("root");
    expect(drive.children).toHaveLength(1);
    expect(drive.children[0].children.map(node => node.name)).toEqual(["Nested", "Other", "Project"]);
    expect(roots.find(node => node.name === "\\\\server\\share")?.children[0].name).toBe("Child");
  });

  it("retains unreadable path labels literally and deduplicates repeated pages", () => {
    const item = workspaceItem("unknown", "unresolved-path");
    const roots = buildWorkspaceTree([item, item]);
    expect(roots).toHaveLength(1);
    expect(roots[0].name).toBe("unresolved-path");
    expect(roots[0].workspaces).toHaveLength(1);
  });
});
