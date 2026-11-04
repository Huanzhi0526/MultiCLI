# 已知限制（0.1.0 Beta / S07 已验收）

本文件只写**有证据的边界**。每条都说明适用范围与证据来源；没有测过的项写 `NOT_RUN`，不写成“应该可以”。
更细的逐项版本矩阵见 [`../COMPATIBILITY.md`](../COMPATIBILITY.md)，阶段验收见
[`execution/`](execution/)。

## 1. 平台与安装

- 只支持 **Windows 11 x64**、普通用户权限。没有 macOS / Linux 构建，也没有 ARM64 产物。
- 安装包**未签名**：Windows SmartScreen 会提示未知发布者。本应用不会要求关闭 Defender 或降低系统策略。
- 安装目录 `%LOCALAPPDATA%\MultiCLI Workbench`，数据目录 `%LOCALAPPDATA%\MultiCLI\<profile>`。两者分离是
  0.1.0 的变更；S00 的旧基线包（`MultiCLI`，仅探针）不会被本包覆盖，需要时请手动卸载旧包（预期只删
  自己的文件、保留数据目录；**S07 未实测旧包卸载**，真实卸载列 `NOT_RUN`）。
- 本机隔离目录真实安装、版本夹具升级、重新打开和默认卸载已由主管验证；夹具使用同一候选代码，仅安装版本不同，**不证明历史版本迁移**。没有现成干净 VM / Windows Sandbox，干净 VM 安装仍 `NOT_RUN`，未改 Hyper-V/安全设置。
- 本地安装器由 Tauri CLI **2.12.3** 调用其 NSIS 缓存生成；本机 `makensis` 为 **v3.13**，
  SHA256 `65343C225EC88393CC4453B0963CAF63F763F42F48CB55736D51893FF2E29D4A`。该缓存来源为 Tauri
  工具链，**主管已与 Tauri 官方指定 NSIS3.13 压缩包逐字节核对 441 项且一致**（2026-11-04）；专用插件 DLL 官方摘要也匹配；开发者没有从第三方镜像替换安装工具。

## 2. 产品覆盖

- **三核心**：Codex CLI 0.166.0、Claude Code 2.1.153、CodeBuddy Code 2.179.3 有真实历史、结构化交互、
  权限拒绝/允许、取消、退出、同 Session 继续的**有限样本**。样本不等于全版本承诺；升级 CLI 后能力可能变化。
- **扩展族**：真实发现 + PTY 覆盖 2 款——TRAE 0.209.1（本机）与 Gemini 0.66.0（本轮隔离安装到
  `scratch/S06/cli-fixture`，只在验收子进程 PATH 可见）。Gemini 只验证了发现与 `version` / `help` 启动，
  **没有登录、推理、历史或额度证据**；OpenCode / Qwen / Copilot / Kimi / Aider 未安装，`NOT_RUN`。
  Copilot 在本机只有 IDE 的 `copilot-shim`，不算独立 CLI 支持。
- 额度：只有 Codex 有已验证的官方机器可读出口。Claude 只有本地转录 token，余额不可读；CodeBuddy、
  Gemini、OpenCode、Qwen、Copilot、Kimi、Aider、TRAE 均 `unsupported`，只显示官方人工入口。
- 真实 429 / 真实断网 / 多 bucket 的额度场景仍为 `NOT_RUN`（由合成 stdio 对端与代码路径覆盖）。
- 结构化运行尚未取得真实样本的面：`updatedInput`、超大工具输出、各产品专有的 interruption 面。这些保持
  `unverified`，不写成“支持”。

## 3. 历史与搜索

- 历史预算（S03 验收值，未放宽）：压缩源物理 128 MiB / 解压 256 MiB；单文件事务 1.5 秒；完整性复核 15 分钟
  冷却并分轮推进；30 秒补扫、无文件监听。同 size/mtime 的改写不保证立即可见。
- 独立基准：10,000 会话 / 100,000 消息、暖缓存，宽泛过滤与限制条数搜索 p95 ≤ 1 秒（最慢 623.752 ms）。
  只测同步 SQL 与映射，**不含 Tauri IPC / React 渲染**；不是任意机器或任意规模的承诺。
- 历史不伪造 Run 或协议连接；“继续”按钮只做目标/冲突解析，真实执行属结构化运行。

## 4. 进程与终端

- 外部进程按固定间隔采样，短于间隔的进程可能漏见；**不是审计级全量记录**。
- 每终端缓冲初始 4 MiB、日志 20 MiB，可在界面调整；8 终端 × 100 KiB/s 的回显 p95 目标与 20 轮资源检查
  以主管 [S07 最终验收](execution/S07-ACCEPTANCE.md)为准：含 IPC/xterm 的80样本p95=42.60ms；20轮为同速率3.5秒负载/轮，不是20轮各60秒。只代表本机与本构建。
- 关闭窗口到托盘继续运行；**崩溃不保活**，下次启动不会接管上次遗留的终端。
- `.ps1` 目标要求 PowerShell 7.3+；`.cmd` 走生成的安全 trampoline；不执行 shim 内的任意脚本。
- 没有 psmux / SSH / 远程管理 / 网页远控；这些不在首版范围。

## 5. 数据与备份

- 备份只包含本 profile 的 SQLite 数据库；其中包含本应用已索引的会话与消息内容，不含 WebView 缓存、
  诊断导出、CLI 凭据、CLI 原始历史文件或工作区文件。分享备份前请按敏感数据对待。
- **终端日志是终端内容的本地回放**：每终端 20 MiB 的 `.log` 保存该终端实际输出的原始字节，位于本
  profile 的 `logs/` 下，按终端轮转、可随时删除；它不进入诊断导出、备份包或任何网络请求。若终端里
  出现过敏感内容，请自行删除该目录。
- 恢复**需要重启应用**：计划恢复只写标记，下次启动、数据库打开前才替换；替换前自动保留
  `backups/pre-restore-*` 副本。没有定时备份、云同步或跨机器迁移。
- 数据库 schema 当前为 **v7**。来自更新 schema 的备份被拒绝（不能降级）；更旧 schema 的备份会在下次启动时
  走既有事务迁移。
- 诊断导出是固定白名单，但仍可能包含版本、schema 与计数；分享前请自行确认。

## 6. 语言与可访问性

- 主界面文案支持中文 / English / 한국어；**托盘菜单是原生菜单，固定中文**（`显示主窗口` / `退出 MultiCLI`）。
- 终端内来自 CLI 的输出由各 CLI 自己决定语言，本应用不翻译。
- 界面交互遵循既有 UI/UX 规范（焦点环、`aria-*`、`prefers-reduced-motion`）；没有做完整的读屏器人工走查
  （`NOT_RUN`）。

## 7. 网络与隐私

- 本应用没有任何自己的远程监听、relay、遥测或自动更新发布点；启动后不监听端口，`createUpdaterArtifacts=false`。
- CLI 自身按官方方式联网不属于远程管理，也不被本应用代理或记录。
- 不抓 cookie、不复制浏览器存储、不写入 CLI 全局配置；`MULTICLI_DATA_ROOT` 只是本地验证用的可选覆盖，
  必须给绝对路径，默认不生效。

## 8. 验证状态

- 统一入口 `scripts/verify.ps1`：未执行的范围打印 `NOT_RUN`，失败退出码为 1。
- 受测产品代码 `ab358061267d9bfe7b762c860ad9c22da7d644c0` 已推送，Windows CI `41776922014` 成功；本机真实链路与云端检查分开记录。DeepSeek 不做 Git 写入。
- 最终安装包已由主管从上述干净产品提交构建；安装器SHA256 `C48799203F10C6D293E6AB3357ED0F55EC65E7EAE091AB99F603D13C3E47F168`，未签名，未公开发布。正式安装包不能使用私有性能插桩exe替代。
- Windows真实文件symlink夹具因缺权限 `NOT_RUN`；没有为了测试开启Developer Mode或修改安全策略。
