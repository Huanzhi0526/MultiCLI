/**
 * Release notes shown in the release-notes center tab (ReleaseNotesPane).
 * Newest first; add an entry at release time. Content is bilingual —
 * the tab shows both when available, ordered by the active UI language.
 */

/** Repo the release-notes tab links to for the Star banner / about page. */
export const GITHUB_REPO_URL = "https://github.com/Huanzhi0526/MultiCLI";

export interface ChangelogEntry {
  version: string;
  date: string;
  content: {
    en: string;
    zh: string;
  };
}

/** 版本号比较：两边都去掉可选前缀 v，忽略大小写。 */
export function sameVersion(a: string, b: string): boolean {
  return a.trim().replace(/^v/i, "").toLowerCase() === b.trim().replace(/^v/i, "").toLowerCase();
}

/** 本地版本记录里与 `version` 同名的条目；没有就是没有——不回落相邻版本，
 *  否则会把 v1.0.7 的说明挂在 v1.0.8 的标题下。 */
export function changelogEntryFor(version: string): ChangelogEntry | undefined {
  return CHANGELOG_DATA.find((entry) => sameVersion(entry.version, version));
}

export const CHANGELOG_DATA: ChangelogEntry[] = [
  {
    version: "0.1.0",
    date: "2026-11-04",
    content: {
      zh: `MultiCLI 首个版本。
- Windows本地工作区、Agent与会话导航，三核心只读历史索引和搜索。
- CLI安装与外部进程发现、自有ConPTY终端与结构化运行。
- Codex、Claude、CodeBuddy有限版本样本的权限、工具、取消与继续。
- 有来源的用量、账户池、自定义CLI、本profile备份恢复和诊断导出。
- 用户级未签名安装包；覆盖和限制以仓库兼容性与版本说明为准。
- 项目进入功能更新、体验优化、性能与运维维护阶段。`,
      en: `First MultiCLI version.
- Local Windows workspace, agent and session navigation with read-only history search.
- CLI discovery, observed external processes, owned ConPTY terminals and structured runs.
- Permission, tool, cancellation and resume flows observed in limited Codex, Claude and CodeBuddy versions.
- Sourced usage data, account pools, custom CLIs, profile backup/restore and diagnostics.
- Unsigned per-user installer; coverage and limitations are documented in the repository.
- Ongoing work focuses on features, user experience, performance and operations.`,
    },
  },
];
