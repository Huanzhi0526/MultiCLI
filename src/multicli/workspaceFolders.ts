import type { WorkspaceListItem } from "./types";

export interface WorkspaceFolder {
  key: string;
  name: string;
  path: string;
  children: WorkspaceFolder[];
  workspaces: WorkspaceListItem[];
}

/** Build navigation from registered paths only; never enumerate the filesystem.
 * Host and Windows root are part of every key. UNC shares stay independent.
 * Preserve spelling for display, while Windows comparisons ignore casing.
 */
function pathSegments(value: string): { names: string[]; paths: string[]; windows: boolean } {
  const extended = value.replace(/^\\\\\?\\UNC\\/i, "\\\\").replace(/^\\\\\?\\/, "");
  const windows = /^[a-z]:[\\/]|^\\\\/i.test(extended);
  const path = windows ? extended.replace(/\//g, "\\") : extended;
  const unc = /^\\\\([^\\]+)\\([^\\]+)(?:\\|$)/.exec(path);
  const drive = /^([a-z]:)\\/i.exec(path);
  let names: string[];
  let root: string;
  let separator: string;
  if (unc) {
    root = `\\\\${unc[1]}\\${unc[2]}`;
    names = [root, ...path.slice(unc[0].length).split("\\").filter(Boolean)];
    separator = "\\";
  } else if (drive) {
    root = `${drive[1].toUpperCase()}\\`;
    names = [root, ...path.slice(drive[0].length).split("\\").filter(Boolean)];
    separator = "\\";
  } else if (path.startsWith("/")) {
    root = "/";
    names = [root, ...path.slice(1).split("/").filter(Boolean)];
    separator = "/";
  } else {
    // Do not guess a root for an unrecognized or unresolved path.
    return { names: [value], paths: [value], windows: false };
  }
  const paths = [root];
  for (const name of names.slice(1)) {
    const parent = paths[paths.length - 1];
    paths.push(`${parent}${parent.endsWith(separator) ? "" : separator}${name}`);
  }
  return { names, paths, windows };
}

export function buildWorkspaceTree(items: WorkspaceListItem[]): WorkspaceFolder[] {
  const roots: WorkspaceFolder[] = [];
  const folders = new Map<string, WorkspaceFolder>();
  const seen = new Set<string>();
  for (const item of items) {
    if (seen.has(item.workspace.id)) continue;
    seen.add(item.workspace.id);
    const { names, paths, windows } = pathSegments(item.workspace.path.displayPath);
    let siblings = roots;
    for (let index = 0; index < names.length; index++) {
      const path = paths[index];
      const key = JSON.stringify([item.workspace.path.hostId, windows ? path.toLowerCase() : path]);
      let folder = folders.get(key);
      if (!folder) {
        folder = { key, name: names[index], path, children: [], workspaces: [] };
        folders.set(key, folder);
        siblings.push(folder);
      }
      if (index === names.length - 1) folder.workspaces.push(item);
      siblings = folder.children;
    }
  }
  const sort = (nodes: WorkspaceFolder[]) => {
    nodes.sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }) || a.key.localeCompare(b.key));
    for (const node of nodes) sort(node.children);
  };
  sort(roots);
  return roots;
}
