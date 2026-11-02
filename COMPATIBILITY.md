# MultiCLI 兼容性实测表

更新至 2026-11-02。版本/协议列保留 2026-11-01 的 S00 CLI 探针；历史列更新为 S03 独立副本实测。S01 的实际 IPC、双向导航、路径/worktree 与诊断已验收，但会话/能力为显式合成 fixture，不升级下表任何 CLI 兼容性结论。版本/帮助、自报握手能力、fixture、真实产品端到端分别记录；未知写 UNVERIFIED/NOT_RUN，不用一个勾代替。

| 产品 | 本机版本/入口证据 | 无推理协议探针 | 历史/搜索 | 结构化交互/权限/继续/取消 | 产品 PTY | 额度 |
|---|---|---|---|---|---|---|
| Codex CLI | 0.166.0，exe，version/help PASS | 独立 CODEX_HOME，initialize + initialized PASS；321 个 JSON schema 已导出 | PASS：S03 观察格式/只读副本，见下方范围 | NOT_RUN | NOT_RUN | schema 确有 account/rateLimits/read；真实账户 NOT_RUN |
| Claude Code | 2.1.153，exe，version/help PASS | 帮助声明 print、流格式等；协议真实启动 NOT_RUN | PASS：S03 观察格式/只读副本，见下方范围 | NOT_RUN | NOT_RUN | NOT_RUN |
| CodeBuddy Code | 2.179.0，npm Node 入口，version/help PASS | 独立 CONFIG_DIR/DATA_ROOT，ACP v1 initialize PASS；自报 loadSession=true，仅声明 | PASS：S03 观察格式/只读副本，见下方范围 | NOT_RUN | NOT_RUN | NOT_RUN |
| Gemini CLI | 未发现 PATH 命令，尚未完整安装扫描 | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN |
| OpenCode | 未发现 PATH 命令，尚未完整安装扫描 | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN |
| Qwen Code | 未发现 PATH 命令，尚未完整安装扫描 | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN |
| GitHub Copilot CLI | PATH 发现 VS Code copilot-shim/copilot.exe；产品/版本待确认，不能视为独立 CLI 已支持 | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN |
| Kimi Code CLI | 未发现 PATH 命令，尚未完整安装扫描 | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN |
| Aider | 未发现 PATH 命令，尚未完整安装扫描 | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN |
| TRAE CLI（增补候选） | 原生 traex.exe；0.209.1，public installation，version/help PASS | help 声明 acp/app-server，实际握手 NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN | NOT_RUN |

探针没有发送 prompt，没有 session/new、thread/start 或 resume；均不证明认证可用、权限拒绝成立、工具事件完整或终端可交互。Codex/CodeBuddy 握手进程均已关闭，只处理本次自有进程。Claude 原生能力按安装帮助继续调查，不推断不存在的 SDK。

证据在项目根 scratch/s00/，不提交原始用户数据。Codex schema 来自 0.166.0 二进制，接口指导：[官方 App Server 文档](https://learn.chatgpt.com/docs/app-server)。CodeBuddy 环境目录变量已对照安装包 dist/codebuddy.js，未改现有 CLI 配置。TRAE 包装器初次参数转发探针失败，后直接调用包装器所指原生 exe，失败与修正记录均保留。

只读目录元信息：Codex sessions、Claude projects、CodeBuddy sessions/projects 候选根存在且可列目录；该 S00 时点内容解析全部 NOT_RUN；S03 当前实测见末段。TRAE 已检查的常见候选未发现，其他来源仍未知。这些目录探针不授予历史兼容性。

S00 平台基线另经真实 Windows 桌面、dev/test/prod 与安装版 cmd.exe ConPTY 按钮验证；四项结果均 true。这不将上表任一产品 PTY、历史、结构化交互或额度从 NOT_RUN 改为 PASS。


S02 主管独立验收 ACCEPTED（2026-11-02）：真实 Windows 后端与实际产品界面在隔离 ConPTY/PowerShell 中发现 Codex、Claude、CodeBuddy；每款两组长存活样本分别为 3238/947、3247/967、3187/881 ms。实际 cmd→Node 链、两个普通进程负例、完整创建时间身份、stale/error/重试恢复、设置写入恢复及窗口退出不影响外部进程均通过。安装页准确区分可信三核心与未知/残留候选。拒绝访问/PID 竞争另有确定性反例，真实升级/受保护进程/长时性能未测；不是 p95 或九族覆盖结果。此结果不提升上表历史、聊天、权限、产品 PTY 或额度结论。


S03（2026-11-02）整阶段独立验收 ACCEPTED：三核心四个真实文件（Codex 未压缩/压缩各一，Claude/CodeBuddy 各一）只读副本，共4会话/136消息；完整正文、顺序、身份与重启一致，原始四文件 SHA256 均未变。隔离副本追加/替换后137消息，真实 HistoryEngine 重建保留全文/身份/备注/标签。普通用户实窗历史、双向入口/分页、中文搜索/定位与失败恢复通过；独立10k会话/100k消息暖缓存查询 p95 全部≤1秒。

此通过限观察格式及明确预算：压缩物理128MiB/解压256MiB、单文件1.5秒、完整性15分钟冷却并分轮推进；不保证任意版本/无限规模/即时同mtime变更。历史不伪造 Run/协议连接，继续按钮仅做已知/未知冲突检查，真实恢复执行与权限仍属 S05 NOT_RUN。其他产品历史未测，额度及产品交互 PTY 也未升级。完整独立证据和限制见 [S03 验收](docs/execution/S03-ACCEPTANCE.md)。
