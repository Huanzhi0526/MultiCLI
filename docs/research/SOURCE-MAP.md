# 当前源码地图：0.1.0

| 领域 | 当前入口 |
|---|---|
| React挂载与主导航 | src/main.tsx、src/multicli/App.tsx |
| 会话/工作区/Agent | src/multicli/SessionBrowser.tsx、src-tauri/src/multicli/{service,repo,path,domain}.rs |
| IPC类型与客户端 | src/multicli/{types,api}.ts、src-tauri/src/multicli_bootstrap.rs |
| 安装与外部进程 | src/multicli/discovery/、src-tauri/src/multicli/discovery/ |
| 历史与搜索 | src/multicli/history/、src-tauri/src/multicli/history/ |
| 托管终端 | src/multicli/terminal/、src-tauri/src/multicli/terminal/ |
| 结构化运行 | src/multicli/runtime/、src-tauri/src/multicli/runtime/，adapters/{codex,claude,codebuddy}.rs |
| 用量与账户池 | src/multicli/usage/、src-tauri/src/multicli/usage/ |
| 自定义CLI | src/multicli/custom/、src-tauri/src/multicli/custom.rs |
| 备份恢复 | src/multicli/data/DataView.tsx、src-tauri/src/multicli/backup.rs |
| SQLite与迁移 | src-tauri/src/multicli/store.rs，schema v7 |
| 诊断与脱敏 | src-tauri/src/multicli/diagnostics.rs、discovery/redact.rs |
| 国际化 | src/i18n/multicli.ts |
| 当前构建检查 | scripts/verify.ps1、scripts/multicli/{verify,enter-env}.ps1 |
| 安装标识与卸载钩子 | src-tauri/tauri.conf.json、src-tauri/windows/nsis-hooks.nsh |

细分文件以当前目录为准。前端DTO、IPC消费和Rust产出需同时核对；不要照搬上游同名界面路径。
保留的src/features、src/App.tsx及旧bootstrap属于继承树，当前MultiCLI挂载图没有通过它们恢复上游功能。
