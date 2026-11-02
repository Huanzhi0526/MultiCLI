# 实际源码映射（S00–S02）

固定上游 `6c46345a727e7c1b57e0dacecb4fe063d88ee8dc`；以下路径相对 app。现存文件与拟新增路径明确分开。

| 领域 | 已存在位置 | 复用/隔离决定 |
|---|---|---|
| 二进制与启动 | src-tauri/src/main.rs → lib.rs::run → multicli_bootstrap.rs | 当前只运行新隔离启动；旧 startup 不可恢复 |
| 应用数据 | src-tauri/src/paths.rs；multicli_bootstrap.rs | 新 app_home；provider/home 读取与应用数据分离 |
| 原数据库 | src-tauri/src/db.rs | 参考迁移/查询方式；不自动打开旧库，S01 建独立领域库 |
| 引擎公共层 | src-tauri/src/engine/mod.rs、events.rs、registry.rs、job.rs、resolve.rs | Run/Process 新领域模型不能冒用旧 session/PID 字段 |
| Codex 协议 | src-tauri/src/engine/codex_app.rs、codex_read_only.rs、codex_usage.rs | 对照实际 0.166.0 schema；现存自动拒绝策略须 S05 明确改造 |
| Claude | src-tauri/src/engine/claude.rs、claude_channel.rs | 对照安装版帮助及真实权限往返再复用 |
| CodeBuddy | src-tauri/src/engine/codebuddy.rs | 不能凭文件名认定已具备目标 ACP 权限链路；S05 单独验证/实现 |
| PTY | src-tauri/src/terminal.rs、engine/job.rs；src/features/terminal/TerminalView.tsx、store.ts | 保留后端/组件；S00 原生探针在 multicli_bootstrap，S04 再整合生命周期 |
| 原始历史 | src-tauri/src/history/scanner.rs、reader.rs、search.rs、edit.rs | 解析可参考；edit 不可注册；无启动自动扫描 |
| 前端入口 | src/main.tsx → src/multicli/App.tsx | 当前产品导航；BaselineApp 保留未挂载 |
| 原聊天/设置 | src/features/chat/ChatPage.tsx、store.ts；src/features/settings/SettingsPage.tsx | 保留组件，当前未挂载；不可整体恢复超范围入口 |
| 原前端副作用 | src/bootstrap.tsx、src/App.tsx、src/lib/analytics.ts | 保留供映射，当前入口不导入/运行 |
| 用量参考 | src-tauri/src/usage_limits.rs、usage_stats.rs、engine/codex_usage.rs | S06 对照官方接口，不能混成订阅余额 |
| 远程/电脑控制/插件 | src-tauri/src/web.rs、relay.rs、computer_use.rs、plugins/、dsh_host.rs | 不在当前 IPC 注册表或启动链；没有本版实现任务 |

S01 已验收模块：`src-tauri/src/multicli/{domain,error,store,repo,service,path,diagnostics}.rs`，公开入口 MultiCliService，独立 multicli.db（S01 schema v2；S02 已验收 v3）。path.rs 处理真实路径/worktree，diagnostics.rs 匿名白名单导出，tests.rs 正式回归。

六个产品 IPC、懒初始化与失败重试在 `src-tauri/src/multicli_bootstrap.rs`，没有 multicli/ipc.rs。前端为 `src/multicli/{App.tsx,components.tsx,api.ts,types.ts,navigation.ts,usePagedResource.ts}`，文案 `src/i18n/multicli.ts`，不走旧 src/App.tsx。显式合成工具 `src-tauri/examples/multicli_fixture.rs` 不由产品启动调用。冻结 schema 未改。

S02 已经独立代码、真实 Windows 后端与实窗验收 ACCEPTED。后端 `src-tauri/src/multicli/discovery/` 已存在：signature/wrapper/detect/probe、winproc/classify/snapshot、engine/view/tests。四个发现/设置 IPC 仍在 multicli_bootstrap。schema v3 扩展安装身份并添加 discovery_processes，迁移在 store/repo/service 内。

前端 `src/multicli/SessionBrowser.tsx` 保留 S01 三栏，App.tsx 顶部切换；`src/multicli/discovery/{InstallationsView,ProcessesView,controls,useDiscoveryScan}` 已存在，组件后缀 tsx、hook 后缀 ts。新接口详见 docs/contracts/discovery-v1.md，三份原冻结 schema 未改。没有恢复旧 startup/history/terminal。
