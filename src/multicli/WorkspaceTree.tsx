import { useMemo } from "react";
import { ChevronDown, ChevronRight, Folder, FolderOpen } from "lucide-react";
import { useTranslation } from "react-i18next";
import { buildWorkspaceTree, type WorkspaceFolder } from "./workspaceFolders";
import type { WorkspaceListItem } from "./types";

export default function WorkspaceTree({ items, selectedId, expanded, onToggle, onSelect }: {
  items: WorkspaceListItem[];
  selectedId: string | null;
  expanded: ReadonlySet<string>;
  onToggle: (key: string) => void;
  onSelect: (id: string) => void;
}) {
  const { t } = useTranslation("multicli");
  const roots = useMemo(() => buildWorkspaceTree(items), [items]);
  const render = (node: WorkspaceFolder) => {
    const open = expanded.has(node.key);
    const branch = node.children.length > 0;
    const Icon = open && branch ? FolderOpen : Folder;
    const Chevron = open ? ChevronDown : ChevronRight;
    const selected = node.workspaces.some(item => item.workspace.id === selectedId);
    return <li key={node.key}>
      <div className={`mc-folder-row ${selected ? "mc-folder-selected" : ""}`}>
        {branch ? <button type="button" className="mc-folder-toggle" aria-expanded={open}
          aria-label={t(open ? "workspaceCollapse" : "workspaceExpand", { name: node.name })}
          title={node.path} onClick={() => onToggle(node.key)} data-testid="workspace-folder-toggle">
          <Chevron aria-hidden="true" />
        </button> : <span className="mc-folder-toggle" aria-hidden="true" />}
        {node.workspaces.length === 0 ? <button type="button" className="mc-folder-name" title={node.path}
          aria-expanded={open} onClick={() => onToggle(node.key)}>
          <Icon aria-hidden="true" /><span>{node.name}</span>
        </button> : <div className="min-w-0 flex-1">
          {node.workspaces.map(item => <button type="button" key={item.workspace.id} className="mc-folder-name"
            data-testid={`workspace-row-${item.workspace.id}`} aria-current={item.workspace.id === selectedId ? "true" : undefined}
            title={`${item.workspace.path.displayPath}\n${t("sessionCount", { count: item.sessionCount })}${item.workspace.path.canonicalPath === null ? ` · ${t("pathUnresolved")}` : ""}`}
            onClick={() => onSelect(item.workspace.id)}>
            <Icon aria-hidden="true" /><span>{node.name}</span>
            <span className="mc-folder-count" aria-label={t("sessionCount", { count: item.sessionCount })}>{item.sessionCount}</span>
          </button>)}
        </div>}
      </div>
      {branch && open ? <ul className="mc-folder-children">{node.children.map(render)}</ul> : null}
    </li>;
  };
  return <ul className="mc-workspace-tree" aria-label={t("workspaceFolders")} data-testid="workspace-folder-tree">{roots.map(render)}</ul>;
}
