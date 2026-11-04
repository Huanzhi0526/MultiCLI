# MultiCLI 维护交接

更新：2026-11-04。当前版本：**0.1.0**。本文件面向后续接管的新功能、体验、性能和运维主管；不是初期施工任务清单。

## 项目是什么

MultiCLI 是纯本地 Windows 多 AI CLI 工作台，采用 Tauri 2、React/TypeScript、Rust、SQLite WAL、xterm 与 ConPTY。
用户从工作区或 Agent 两个方向访问同一会话，搜索只读历史，在工作台创建自有终端或结构化运行，并查看有真实来源的用量信息。
主要能力已完成并形成 0.1.0 基线，当前应围绕实际使用反馈持续维护。

三类来源需要一直区分：managed 是工作台自己创建的进程；external-observed 是只读外部观察；history-only 是历史记录，不代表原进程仍在运行。
外部观察没有通用输入输出接管权。原生/ACP/PTY 是通信方式，不是进程所有权。

## 当前可依赖的基线

- Codex0.166.0、Claude2.1.153、CodeBuddy2.179.3：已观察到真实新建、权限拒绝/允许、工具、取消、退出和同会话继续；只覆盖所列版本的有限样本。
- 三核心历史只读索引与搜索；九产品族识别和未知状态；TRAE0.209.1/Gemini0.66.0只有有限发现与PTY证据。
- Codex官方额度、Claude本地转录统计、自定义CLI、本profile备份恢复与诊断导出。
- Windows用户级未签名安装器已在本机隔离目录验证安装、版本夹具升级、重新打开及默认保留数据卸载。
- 当前没有已确认、尚未解决的阻断缺陷；用户正在开始日常试用。没有正在派发的开发任务。

实际覆盖、版本与未测边界以 COMPATIBILITY.md、docs/KNOWN-LIMITATIONS.md 和 docs/releases/0.1.0.md 为准。
0.1.0产品代码基线为 ab358061267d9bfe7b762c860ad9c22da7d644c0；后续文档整理及版本元数据另有提交。不要把版本名当成全环境兼容保证。

## 新对话接手后先做什么

1. 核对当前分支、HEAD、未提交改动和 STATUS.md，确认是否已有其他 Agent 在写同一目录。
2. 明确用户这次希望解决的问题：功能、界面、性能、构建还是运维。没有具体任务时，先收集试用反馈，不自行扩展范围。
3. 根据 docs/research/SOURCE-MAP.md 阅读真正的产品入口，定位负责该行为的模块。
4. 有明确目标后完成修改、按影响验证，并回写当前状态和有用的长期文档。无需恢复任何初期派工/回交流程。

## 维护职责与优先方向

当前主管负责理解实际问题、控制修改范围、维护数据和进程边界、判断验证覆盖，以及留下下一位可接手的结果。可以由 Claude、Codex 或用户选定的其他 Agent 执行，不固定角色与模型。

建议优先收集并处理：
- 首次启动、历史来源启用、目标选择和错误提示是否清楚。
- 大量历史、长输出、多个终端、页面切换与后台运行的实际性能。
- CLI升级造成的协议、识别、历史格式或额度字段变化。
- Windows安装、升级、故障恢复和长期日志/缓存管理。

这些方向尚未开工。远程管理、SSH、终端崩溃保活、云同步和自动更新均属于新的产品决策，不默认纳入当前维护。

## 必须知道的源码与数据边界

当前前端：src/main.tsx → src/multicli/App.tsx。当前原生启动和IPC：src-tauri/src/multicli_bootstrap.rs。
业务实现：src-tauri/src/multicli/；DTO与API消费：src/multicli/types.ts、api.ts；文案：src/i18n/multicli.ts。
其他大量源码来自固定Desktop CC GUI底座，保留用于复用与来源追踪，不代表当前启用。不要恢复旧bootstrap来“补功能”。

release默认数据：%LOCALAPPDATA%/MultiCLI/prod；debug默认dev；test独立。MULTICLI_DATA_ROOT仅作进程级绝对路径覆盖。
索引库包含会话正文，终端日志含原始输出，备份也可能包含正文；这些不提交Git。诊断导出是白名单摘要。
恢复计划下次启动才应用；排错不能只保存主DB而丢弃WAL/SHM。详见运维说明。

## 跨设备与GitHub协作

GitHub是源码与长期文档的共同来源，正式仓库根已包含本文件、AGENTS.md、CLAUDE.md、STATUS.md和CONTRIBUTING.md。
不同设备克隆后读取这些文件即可建立上下文，无需上传原聊天或本机开发资料。工作进展写入分支/PR和STATUS，账号与真实会话保持本地。
Windows构建与检查由 .github/workflows/multicli-windows.yml 提供；Linux上的前端检查不替代Windows原生验证。
版本标签使用 multicli-v0.1.0；继承的上游v0.1.0不是本产品版本，不覆盖它。

本次用户已授权整理、提交并推送这些文档；后续任务的Git写入、合并或公开发布按当次授权执行。没有创建公开Release。

## 本地工作区补充

用户本机的父目录含app（正式Git仓库）、archive（旧施工资料本地归档）、references（固定只读参考）、scratch（工具、原始证据、安装包）和缓存。
这些目录不要求在其他设备存在。编译缓存由用户明确选择保留；不要为了“整理”自动删除、移动工具或重置真实profile。
旧施工计划与阶段记录已退出当前仓库目录；需要追溯时使用Git历史或本机archive，它们不再是任务指令。

## 文档导航

- docs/DEVELOPMENT.md：Windows工具链、运行与按需检查。
- docs/ARCHITECTURE.md、docs/research/SOURCE-MAP.md：架构与定位。
- docs/OPERATIONS.md：备份、排错、升级、卸载与清理边界。
- docs/PERFORMANCE.md：测量口径和已有数据。
- docs/ROADMAP.md：待用户选择的后续方向。
