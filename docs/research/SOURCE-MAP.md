# 实际源码映射（S00）

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
| 前端入口 | src/main.tsx、src/multicli/BaselineApp.tsx | 临时真实诊断页，S01 实现产品导航 |
| 原聊天/设置 | src/features/chat/ChatPage.tsx、store.ts；src/features/settings/SettingsPage.tsx | 保留组件，当前未挂载；不可整体恢复超范围入口 |
| 原前端副作用 | src/bootstrap.tsx、src/App.tsx、src/lib/analytics.ts | 保留供映射，当前入口不导入/运行 |
| 用量参考 | src-tauri/src/usage_limits.rs、usage_stats.rs、engine/codex_usage.rs | S06 对照官方接口，不能混成订阅余额 |
| 远程/电脑控制/插件 | src-tauri/src/web.rs、relay.rs、computer_use.rs、plugins/、dsh_host.rs | 不在当前 IPC 注册表或启动链；没有本版实现任务 |

S01 数据层现已存在：`src-tauri/src/multicli/{domain,error,store,repo,service}.rs`，公开入口 `MultiCliService`，数据库 `multicli.db`，schema version 2。`mod.rs` 公开类型/服务，`tests.rs` 包含正式回归。产品 IPC 和导航尚未实现；`src-tauri/src/multicli/ipc.rs` 与 `src/multicli/{domain,navigation,...}` 是下一阶段包的拟新增路径，不可假定已存在。正式 IPC/领域约定见 docs/contracts/；旧命令没有自动获得 MultiCLI 授权。
