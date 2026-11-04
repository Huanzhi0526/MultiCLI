# MultiCLI

纯本地 Windows 多 CLI 桌面工作台：在一个窗口里查看、搜索和操作多个 AI CLI 的**工作区、Agent、会话、历史、
托管终端、结构化运行和账户用量**。工作台与索引都在本机运行；CLI 仍按各自官方方式联网调用模型，但本应用
不接管外部启动的进程，也不把任何数据上传到自己的服务端。

首版目标：Windows 11 x64、普通用户权限、未签名测试包。当前构建：**0.1.0（S07 源码候选，完整安装/性能验收中）**。

- 当前状态与验收（主管维护）：[`docs/execution/STATUS.md`](docs/execution/STATUS.md)
- 快速上手：[`QUICKSTART.md`](QUICKSTART.md)
- 能力与版本实测（S00–S06 基线；S07-R1 增量见已知限制与交付文档）：[`COMPATIBILITY.md`](COMPATIBILITY.md)
- 已知限制：[`docs/KNOWN-LIMITATIONS.md`](docs/KNOWN-LIMITATIONS.md)
- 接口契约：[`docs/contracts/`](docs/contracts/)
- 源码地图：[`docs/research/SOURCE-MAP.md`](docs/research/SOURCE-MAP.md)
- 底座与环境：[`docs/research/BASELINE.md`](docs/research/BASELINE.md)、[`docs/research/ENVIRONMENT.md`](docs/research/ENVIRONMENT.md)

## 三类来源，三种权力

| 来源 | 本应用能做什么 | 不会做什么 |
|---|---|---|
| **managed**（工作台自己启动） | 通过自有协议或 ConPTY 提供输入/输出、生命周期、取消与退出 | 不声称 CLI 未提供的历史、精确交互状态或余额 |
| **external-observed**（外部程序启动） | 只读发现安装与进程关系、可确认的元信息、历史关联 | 不接管 stdin/stdout、不注入、不劫持、不自动结束外部进程 |
| **history-only**（只读历史） | 索引、搜索、查看；有官方入口时显式继续 | 不声称原进程仍活着、不伪造 Run 或协议连接 |

工作区 → Agent 与 Agent → 工作区打开的是**同一个 Session**、同一组消息与同一组计数。Session、Run、Process
分开存储；进程身份用 `hostId + PID + 创建时间`，不按进程名猜。

## 功能范围

| 能力 | 状态 |
|---|---|
| 工作区 / Agent 双向导航、分页、能力声明、脱敏诊断 | 已交付（S01–S03） |
| 安装发现、外部进程观察、父子链、身份与失败恢复 | 已交付（S02） |
| 三核心只读历史、增量索引、搜索、恢复目标/冲突检查 | 已交付（S03） |
| 托管 ConPTY + xterm、多终端、参数链、托盘/退出、背压与资源上限 | 已交付（S04） |
| 三核心结构化运行：新建/继续、权限往返、工具事件、取消、停止 | 已交付（S05，有限真实样本） |
| 账户用量/额度池、九族 CLI 识别、扩展 PTY、自定义 CLI | 已交付（S06，覆盖有限，见 COMPATIBILITY） |
| 本 profile 备份/恢复、统一验证入口、安装包与使用说明 | 源码已验收，安装/性能门禁待完成（S07） |

**三核心**：Codex CLI 0.166.0、Claude Code 2.1.153、CodeBuddy Code 2.179.3。三款各有真实历史、结构化
交互与继续/取消的有限样本；**样本不等于全版本承诺**，每款的分能力结论以 `COMPATIBILITY.md` 为准。

**九族 + TRAE**：Codex、Claude、CodeBuddy、Gemini、OpenCode、Qwen、Copilot、Kimi、Aider，另加观测型
TRAE。真实安装发现、PTY 启动、历史、额度分别记录；未安装族与未验证入口保持 `NOT_RUN` / `unverified`，
不合并成一个「已支持」。

### 额度与用量

- 只有 **Codex** 有已验证的官方机器可读出口（`codex app-server` 的 `account/read`、
  `account/rateLimits/read`、`account/usage/read`），且只读当前 CLI 已有认证范围内的数据。
- **Claude Code** 只有本地转录 token 统计；余额不可读，`/cost` 需用户手动执行。
- 其余产品没有已验证的机器可读出口：状态是 `unsupported`，并显示官方人工入口，**不显示为 0**。
- 缺失、未知、过期、未认证、限流、失败各自有独立状态；费用估算与真实账单分开；缓存 token 只按提供方
  语义计一次。

## 托盘与退出

- 关闭主窗口**不会退出**：窗口隐藏到托盘，托管终端与结构化运行继续排空。
- 托盘「退出 MultiCLI」与页头「退出」走同一条确认路径：仍持有托管终端时先询问「取消 / 停止并退出」，
  只有确认后才结束**本应用自己启动的**进程树。外部进程不被枚举、不被终止。
- 崩溃不承诺终端保活；下一次启动不会接管上次遗留的进程。

## 数据、备份与卸载

- 应用标识 `local.multicli.workbench`；数据在 `%LOCALAPPDATA%\MultiCLI\{dev,test,prod}`，WebView 数据在
  对应 profile 的 `webview/` 下。debug 默认 `dev`，release 默认 `prod`；`MULTICLI_PROFILE=test` 用于隔离试验。
  验证/便携运行时可用 `MULTICLI_DATA_ROOT=<绝对路径>` 直接指定 profile 根目录（默认不生效，界面会显示实际目录）。
- **首次启动不写入任何演示数据**；会话/工作区来自你在「历史」页显式启用的只读来源。
- 「数据与备份」页可生成本 profile 的 SQLite 快照（`VACUUM INTO` + 完整性检查 + SHA-256），并计划一次
  跨重启恢复；快照包含本应用数据库（含已索引的会话与消息内容），但**不包含** CLI 凭据、CLI 原始历史
  文件或工作区文件。详见 [`docs/contracts/profile-backup-v1.md`](docs/contracts/profile-backup-v1.md)。
- 卸载默认**保留** `%LOCALAPPDATA%\MultiCLI`；手动清理步骤见 [`QUICKSTART.md`](QUICKSTART.md#卸载与清理)。
- 安装目录是 `%LOCALAPPDATA%\MultiCLI Workbench`，与数据目录分离，删除程序不会碰数据。

## 隐私与进程边界

- 没有默认远程监听、旧 relay、遥测、自动更新发布点或 CLI 安装商店；不写入 CLI 全局配置，不复制凭据。
- 诊断导出是固定白名单（版本、schema、计数、匿名化的能力统计），不含正文、账号、绝对路径或数据库文件。
- 外部进程只读观察；本应用只管理有身份凭据的自有进程树。

## 构建与验证

Windows x64；Node 24.16.0；项目 pnpm 10.29.3（Corepack）；Rust 1.99.1 MSVC；MSVC Build Tools + Windows SDK；
WebView2；完整原生 Perl（含 `IPC::Cmd` / `Locale::Maketext::Simple`）。

```powershell
corepack pnpm install --frozen-lockfile --ignore-scripts
./scripts/verify.ps1 -Scope Frontend     # 类型检查 + 前端构建 + 平台边界
./scripts/verify.ps1 -Scope Test         # 前端测试 + 领域/bootstrap/fixture 测试
./scripts/verify.ps1 -Scope Rust         # cargo check --locked
./scripts/verify.ps1 -Scope Package      # 构建未签名 NSIS 安装包
./scripts/verify.ps1 -Scope All          # 依次执行以上全部
```

`scripts/verify.ps1` 是统一入口，内部委托 `scripts/multicli/verify.ps1`。**未执行的范围会打印 `NOT_RUN`**，
不会因为「没跑」而显示绿色；任一范围失败整体退出码为 1。

> 本机验证提示：`pnpm exec <tool>` 依赖 `node_modules/.bin` 在 `PATH` 中；统一入口会自动加入。若自行调用
> 子脚本，请先把它加入 `PATH`，否则会出现「找不到 vitest」这类环境错误。

## 安装包

- 产物：`MultiCLI Workbench_0.1.0_x64-setup.exe`（NSIS，`currentUser`，**未签名**）。
- 独立标识与应用名，不覆盖上游 Desktop CC GUI，也不覆盖旧的 `MultiCLI` 基线探针包。
- 未签名意味着 Windows 可能显示 SmartScreen 提示；本应用**不会**为此降低系统安全策略，也不提供签名证书。
- 安装包必须从已提交源码构建；源码 commit、构建命令与 SHA-256 记录在
  [`docs/execution/deliveries/S07.md`](docs/execution/deliveries/S07.md)、
  [`S07-R1.md`](docs/execution/deliveries/S07-R1.md) 与 `docs/execution/S07-ACCEPTANCE.md`（验收后）。

## 施工方式

Sol 主管、DeepSeek 开发，仅两个角色；本项目不用 Baton。DeepSeek 回交未提交代码并停止写入，Sol 独立验收后
commit/push。原始历史、凭据、数据库、参考仓库、日志和安装产物不进 Git。

## 许可

源自 Desktop CC GUI 固定 v1.2.0 / `6c46345a727e7c1b57e0dacecb4fe063d88ee8dc`，保留 Git 历史与 MIT 许可；
第三方许可见 [`THIRD_PARTY.md`](THIRD_PARTY.md) 与 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)。上游原说明
保留在 [`docs/research/UPSTREAM-README.md`](docs/research/UPSTREAM-README.md)。
