# MultiCLI 托管终端与生命周期契约 v1

适用0.1.0。终端Run为transport=pty、ownership=managed，复用domain/error/events模型；当前整体数据库v7。终端专有视图与命令在本文件说明。

## 1. 范围与不变量

1. **只控制自己创建的进程。** IPC 没有任何接收 PID 的入口；外部 CLI（已观察到的实例）
   无法从这里发送输入、停止或 attach。停止前必须用 `hostId + pid + creationTime` 复核身份。
2. **一个终端 = 一个自有 ConPTY + 一个子进程树 + 一个 Run。** 子进程在汇报 `running`
   之前已进入 Job Object；Job 分配失败则创建直接失败，不允许“没有树约束地跑起来”。
3. **失败不留下成功的 Run，也不留孤儿树。** 创建顺序为：写 `starting` Run → 生成启动计划
   → spawn → Job → 身份 → 链接进程 → `running`。任一步失败：结束自有树、Run 记 `failed`
   并写 `ended_at`。
4. **输出是字节流。** 终端字节不冒充结构化消息、工具事件或权限请求；没有任何协议证据时
   `interactionStatus` 恒为 `unknown`，不因“没有输出”推断 `idle`。
5. **缓冲与日志有界。** 每终端内存环 4 MiB、日志 20 MiB（可配置），丢弃必须可见并计数。
6. **正常启动不安装 CLI、不导入 fixture、不监听远程、不读旧 GUI 数据。**

## 2. 目标（Target）

目标有三类，都来自本机真实存在的对象：

| kind | 选择依据 | 固定内容 |
|---|---|---|
| `shell` | `shell` 字段：`cmd` / `windows-powershell` / `pwsh` | `%SystemRoot%\System32\cmd.exe`、`WindowsPowerShell\v1.0\powershell.exe`，或 PATH 上的 `pwsh.exe` |
| `installation` | `installationId`（`agent_installations` 行） | 该行记录的可执行文件路径（canonical 优先） |
| `custom-cli` | `customCliId`（`custom_clis` 行） | 用户本地导入的可执行文件 canonical 路径 + 预设 argv；见 `usage-v1.md` §5 |

- `shell` 目标**不接受** `args`：它的 argv 是固定 launcher 形式（`cmd /d /q /k chcp 65001>nul`、
  `powershell -NoLogo -NoProfile`），用户文本没有机会变成 shell 命令。
- `installation` 与 `custom-cli` 目标的启动链由扩展名决定，且只有三条：`.exe` → `exe`、
  `.cmd`/`.bat` → `cmd`、`.ps1` → `powershell`。其它扩展名返回 `unsupported`，不做猜测。
- `custom-cli` 的**预设参数排在用户参数之前**，两者一起进入同一条启动链；预设只是 argv 前缀，
  不是新的拼接机制。预设与用户参数合计超过 320 条时创建直接拒绝。
- 自定义 CLI 运行时的 `agent_id` 固定为本地 shell 占位（`local-shell`），
  不会被当成任何产品身份；它的历史与额度恒为 `unsupported`。
- 目标列表 `multicli_terminal_targets` 的 `status` 为 `ready` / `missing` / `unsupported`，
  并带 `reason`；`missing` 表示文件已不存在，选中它会在创建时明确失败。

### 2.1 三条启动链

| chain | 实际 spawn 的进程 | 用户 argv 的去向 |
|---|---|---|
| `exe` | 目标镜像本身 | `argv[]`，由 `CreateProcess` 按 MSVC 规则引用 |
| `cmd` | `cmd.exe /d /s /v:off /c %MULTICLI_CMD_FILE%` | trampoline 内每条参数一个引号 token；固定私有环境变量持有被引号包裹的脚本路径 |
| `powershell` | `pwsh.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File <trampoline.ps1>` | trampoline 内每条参数一个单引号字面量；要求 PowerShell 7.3+，使用 Standard 原生参数转发 |

trampoline 是 `<dataRoot>/terminal/launch/<terminalId>/run.cmd|run.ps1`，创建终端时生成、
关闭终端时删除。它存在的原因是：`cmd.exe` 会重新解析自己的原始命令行，而 `CommandBuilder`
只按 MSVC 规则引用，`cmd /c` 的 payload 无法作为单个 argv 元素安全传递。写成文件后，
命令行只含固定启动结构，用户参数的转义规则可以被单独测试。`.cmd` 脚本路径通过固定环境变量
展开一次，因此含空格、中文、`&`、`%PATH%` 的 profile 仍使用自己的目录，不退到共享目录。

转义规则（在真实 `cmd.exe` / `powershell.exe` 上逐项测过，见 §7 A11）：

- batch token：整体加双引号；`%` → `%%`；`"` → `""`（**不是** `\"`）；以 `\` 结尾（或
  `\` 紧跟 `"`）时把该段 `\` 双写；其余字符（`& | < > ^ ( )`、`!`、空格、中文、emoji）在引号内
  按字面传递。trampoline 以 `setlocal DisableDelayedExpansion` 开头，因此 `!NAME!` 不会在
  trampoline 这一层被延迟展开（若目标脚本自己再开启延迟展开，那属于它自己的行为）。
- 为什么是 `""`：`.cmd` 目标通常是转发 `%*` 的 shim，参数因此要过两次解析（trampoline 一次、
  shim 一次）。`""` 在两次解析里各切换两次引号状态、最终由 CRT 还原成一个 `"`；`\"` 会在
  shim 那次解析提前结束 token，把值里的 `&` 变成第二条命令（实测可执行）。
- **`.cmd`/`.bat` 目标只拒绝两种无法表示的参数**（`unsupported`，`details.stage="batchArgument"`，
  在创建任何资源之前失败）：换行（`cmd` 在换行处结束批处理行），以及 `"` 紧跟 `\`
  （双写引号与反斜杠段在 shim 的第二次解析里会重新分裂）。其余字符一律按字面传递。
- batch 文件用 UTF-8 写入，前两行固定为 ASCII（`@echo off` / `chcp 65001>nul`），因为
  `cmd` 用控制台代码页读取批处理文件；代码页切换之后的 UTF-8 正文才能正确解析。
- PowerShell 字面量：单引号包裹，内部 `'` 双写；换行在单引号字符串内合法。
- 所有 trampoline 留在当前 profile 内；无法创建时返回 `io`，不降级到 ProgramData、系统 Temp
  或其他 profile。启动器私有变量在用户 env delta 之后设置，不能被用户同名变量覆盖。
- `.ps1` 转发到原生程序要求 PowerShell 7.3+ 的 `$PSNativeCommandArgumentPassing='Standard'`。
  Windows PowerShell 5.1 在原生转发中会丢失空参数、引号，因此不用于这条链；本机未找到 `pwsh`
  时返回 `unsupported`，找到旧版本时 trampoline 明确失败，不自动安装。普通 Windows PowerShell
  shell 目标仍可使用，不能据此声称其任意脚本转发保持 argv。

## 3. DTO

所有字段 camelCase、`deny_unknown_fields`。完整定义见 `src/multicli/types.ts` 的终端段与
`src-tauri/src/multicli/terminal/model.rs`。

### 3.1 `TerminalOpenSpec`（创建入参）

```json
{
  "target": { "kind": "shell", "shell": "cmd", "installationId": null },
  "args": ["--flag", "含空格 与&符号"],
  "cwd": "C:\\项目 目录",
  "env": [{ "name": "MULTICLI_SAMPLE", "value": "1" }],
  "cols": 120,
  "rows": 32,
  "title": "可选标题"
}
```

约束：`cwd` 必须是已存在的绝对目录；`args` ≤ 256 项、每项 ≤ 32 KiB 且不含 NUL；`env` ≤ 64 项、
名称唯一（大小写不敏感）、不含 `=` 或控制字符；`cols`/`rows` 在 2–1000。

### 3.2 `TerminalView`（终端状态）

关键字段：

- `terminalId` / `runId` / `sessionId` / `agentId` / `installationId`
- `executable`、`launchChain`、`launchArgv`（**实际 spawn 的 argv**，用于审计）、`args`、`cwd`
- `envNames`：只回显变量名，不回显值
- `state`：`starting` / `running` / `stopping` / `exited` / `failed`
- `interactionStatus`：本契约恒为 `unknown`
- `identity`：`hostId + pid + creationTime`（OS 100ns 精度），未取得时为 `null`
- `job`：`{mode: "kill-on-close"|"unassigned", assigned, reason}`
  （`unassigned` 目前不会被产出：分配 Job 失败会直接让创建失败，而不是“没有约束地跑起来”。
  保留该值是为了让未来“明确受限运行”的策略有一个诚实的表示位，而不是复用 `kill-on-close`
  假装已约束。）
- `buffers`：`bufferCapacityBytes`、`bufferedBytes`、`droppedBytes`、`logPath`、`logCapacityBytes`、
  `logBytes`、`logRotations`、`logDroppedBytes`
- `exit`：`{code, success, endedAt, reported, signal}`；`reported=false` 表示不是 OS 汇报的退出
- `stop`：最近一次停止报告
- `evidence` / `notes`

### 3.3 读取（拉取模型）

`multicli_terminal_read({terminalId, cursor?, maxBytes?})` → `TerminalReadView`

- `seq` 就是**绝对字节偏移**。`fromSeq` / `toSeq` 是本次交付的区间，下一次调用把 `toSeq`
  作为 `cursor`；`cursor=null` 从当前保留的最旧字节开始。
- `maxBytes` 默认 64 KiB、上限 256 KiB；回复永远不会超过它。
- `droppedBefore`：相对调用者游标被丢弃的字节数；`droppedBytes` 是该终端的累计丢弃；
  `historyTruncated=true` 表示这次读取跨过了被丢弃区间。
- `text` 永远是合法 UTF-8。结尾若是不完整的多字节字符或未闭合的控制序列，会被**留到下次**
  一起交付，不会拆开；内部非法字节替换为 U+FFFD，流继续前进。
- 主动推送的只有事件 `multicli://terminal`（见 §4），字节必须拉取。慢 UI 因此只会在有界环里
  落后，不会再堆积无界 IPC 队列。
- 合批发生在读取边界：reader 线程按 8 KiB 持续排空，一次 `read` 把环里最多 64 KiB（请求可到
  256 KiB）合并成一条回复，所以 IPC 次数与 PTY 读取次数不是 1:1。

### 3.4 其他命令

| 命令 | 说明 |
|---|---|
| `multicli_terminal_targets` | 目标列表 + 上限 + 默认 cwd + 固定说明 |
| `multicli_terminal_open` | 创建终端，返回 `TerminalView` |
| `multicli_terminal_list` | 全部终端 + `activeCount` + 上限 |
| `multicli_terminal_write` | `{terminalId, data}`，单次 ≤ 1 MiB；终端不在 `running` 时返回 `conflict` |
| `multicli_terminal_resize` | `{terminalId, cols, rows}` |
| `multicli_terminal_stop` | `{terminalId, mode, graceMs?}`，见 §5 |
| `multicli_terminal_close` | 停止 + 从管理表移除 + 删除 trampoline；未知 id 返回 `removed=false` |
| `multicli_terminal_shutdown_all` | 逐个关闭自有终端，返回 `stopped[]` / `remaining` |
| `multicli_terminal_limits` / `_update_limits` | 读取/修改本 profile 的 `terminal-settings.json` |
| `multicli_terminal_quit_check` | `{activeCount, needsConfirmation, note}` |
| `multicli_app_quit` | `confirm` 必须为 `true`；先温和停止自有终端，再退出 |

## 4. 事件

`multicli://terminal`，payload = `TerminalNotify`：

```json
{ "kind": "output" | "state", "terminalId": "...", "runId": "...", "streamId": "terminal/<id>",
  "seq": 123456, "state": "running", "bufferedBytes": 0, "droppedBytes": 0 }
```

- `kind=output` 在每个终端**最多一条未取走**的通知（读取命令把 dirty 标志清零），
  所以慢 WebView 不会积压通知。
- `kind=state` 在状态迁移时发送。
- 事件不携带输出正文，也不携带任何路径或环境值。

`multicli://quit-requested`：托盘“退出”发出，前端据此显示“取消退出 / 停止自有终端并退出”。

## 5. 生命周期与停止语义

状态机：`starting → running → stopping → exited|failed`，`finalize` 幂等，
先到者胜出，后到的 EOF、重复 stop、迟到的 kill 一律被吸收。

- **退出检测有两条路**：reader 的 EOF 与一个每 150 ms 轮询子进程句柄的看门狗。ConPTY 在子进程
  结束后不一定会立刻给出 EOF（控制台宿主可能仍持着输出管道），只看 EOF 会让“已退出的终端”
  一直显示 `running`；看门狗只读自己拥有的子进程句柄。
- **Run 的状态不可回退**：`set_run_state` 拒绝对已是 `exited`/`failed` 的 Run 写入
  `starting`/`running`/`stopping`，`open` 在链接进程后也会先检查会话是否已经结束。否则一个
  “启动后立刻退出”的目标会留下 `running` + `ended_at` 的自相矛盾记录。
- **终态提交与退出同步**：finalizer 串行执行状态/持久化回调和资源清理；stop 或应用退出观察到终态时，仍等待已开始的 finalizer 完成，避免进程先退出、Run 仍停留在 running。

| mode | 行为 | 说明 |
|---|---|---|
| `interrupt` | 向 PTY 写 `0x03` | 打断前台作业，**不**结束进程；终端保持 `running` |
| `gentle` | 写 `0x03` → 等待 `graceMs`（默认 3000）→ 仍存活则结束自有进程树 | `escalated=true` 表示确实升级 |
| `force` | 直接结束自有进程树（`TerminateJobObject`） | 不发送 Ctrl+C |

- 结束树之前复核身份：`IdentityCheck::Reused` 时**拒绝**并返回 `refusedReason`，
  状态回到 `running`，不碰任何进程。
- 关闭一个终端只影响它自己的 Job；外部同款 CLI 完全不在枚举范围内。
- 主进程退出时，Job 句柄关闭触发内核清扫自有树（`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`）。
- 不实现崩溃保活、psmux、终端持久化。

## 6. 上限与配置

| 项 | 初始值 | 范围 | 位置 |
|---|---|---|---|
| 每终端内存缓冲 | 4 MiB | 256 KiB – 64 MiB | `terminal-settings.json` |
| 每终端日志 | 20 MiB | 1 MiB – 512 MiB | 同上 |
| 同时终端数 | 16 | 固定 | 代码常量 |
| 单次读取 | 64 KiB | ≤ 256 KiB | 请求参数 |
| 单次写入 | — | ≤ 1 MiB | 请求参数 |

日志写在 `<dataRoot>/logs/terminal/<terminalId>.log`，超限时轮转为 `.log.1`；单块超过整个预算
时整块计入 `logDroppedBytes`，不做半截截断。预算覆盖当前文件与一个轮转文件：各使用配置值的一半，
两者合计不超过配置值。跨块 UTF-8/控制序列先保留至完整边界再写入或轮转；未闭合尾部
上限 16 KiB，超限停止此日志并显示错误，EOF 残片计入丢弃。轮转失败停止追加并显示错误，
不重置计数后继续写旧文件。

前端从请求到 xterm `write` 解析回调全程只保持一个在途读取，事件合流且只有一个轮询计时器。
所有唤醒还共享至少 32ms 的单调时钟拉取间隔，使微小持续写入合批而不形成 IPC 忙循环；64KiB 批上限不变，隐藏 pane 继续读取和解析。
输入按到达顺序串行写入，排队加在途 UTF-8 字节最多 1 MiB；超限拒绝该次输入并显示本地化错误。
成功托管会话同时关联其规范化 cwd 工作区；ProcessObservation 记录实际启动镜像，目标脚本仍保留在终端视图。

`multicli_terminal_update_limits` 只影响**之后新建**的终端：已经在跑的终端保持自己创建时的上限，
因为它的环和日志已经按当时的预算分配；界面上的上限文案因此描述的是“新终端的上限”。

## 7. 错误与证据口径

沿用冻结的 `AppError`（`code` / `message` / `retryable` / `details`）。本模块新增的 `details.stage`：

`resolveShell`、`resolveTarget`、`planLaunch`、`createLaunchDir`、`writeTrampoline`、`launchPaths`、
`openpty`、`spawn`、`spawnPid`、`jobAssignment`、`captureIdentity`、`cloneReader`、`takeWriter`、
`spawnReader`、`createLogDir`、`openLog`、`read`、`write`、`resize`、`linkOwnedProcess`、`quit`。

- `unsupported`：目标不可启动（文件缺失、扩展名无启动链、Job 无法约束、trampoline 目录不可用）。
- `conflict`：终端上限已满、终端不在 `running` 时写入、重复停止进行中。
- `not_found`：终端 id 不在管理表、Run 不存在。
- `io`：spawn/读写/日志/设置文件失败，带 `retryable`。

## 8. 明确不做

- 不做外部进程接管、attach、按进程名结束、全量 taskkill。
- 不做终端持久化、崩溃后恢复、psmux。
- 不把终端输出写进消息表或搜索索引。
- 不新增 schema 版本：终端身份复用 `runs` / `processes` / `run_process_links`，
  终端自有状态留在内存与日志里，因此无需 v7 迁移。

## 9. 输出与停止设计

输出通知合流，正文由前端按游标从有界环形缓冲拉取，避免WebView变慢时形成无界推送队列。read清理dirty状态；落后数据的丢弃通过计数呈现。
Ctrl+C、温和停止、强制终止、关闭标签分别表达不同操作，不能合并成含混的单一close动作。
