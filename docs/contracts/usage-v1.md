# usage-v1：账户、额度池与用量读取契约（S06）

状态：`SUBMITTED`（实现方回交，等待主管独立验收）。本文件只描述 S06 新增的接口与行为；
三份冻结 schema（`domain.schema.json`、`error.schema.json`、`events.schema.json`）未改动。
用量相关的 `Run` / `Session` 语义不变：读取额度不创建 Run，也不接管任何 CLI 会话。

## 1. 范围与不变量

1. **账户池去重。** 一个**已确认**账户只有一个 `QuotaPool`（`pool_key = confirmed:<agent>:<fingerprint>`）。
   同一账户的两个安装/两个会话 upsert 同一行，`installationCount` 因此变成 2。身份未确认的池
   `pool_key = unconfirmed:<agent>:<installationId>`，**永不**与已确认池或其他未确认池合并或相加。
2. **缺失不是 0。** 每个测量字段都是可空；`unknown` / `stale` / `unauthenticated` /
   `unsupported` / `rate-limited` / `error` 是显式状态。没有一条代码路径把失败或缺失写成 `0`，
   也不把失败包装成「成功但空」。
3. **额度与 token 是两个对象。** `QuotaWindow`（百分比窗口）与 `TokenUsage`（token 计数）
   分开存储、分开显示；`CreditBalance`、`CostEstimate`、`Billing` 各自独立。费用估算必须
   `estimated=true`，真实账单必须 `estimated=false`，两者不能互换。
4. **缓存 token 只计一次。** 每个 token 样本带 `tokenSemantics`：
   `input-includes-cached`（Codex：`input` 已含缓存，总量 = input + output）或
   `input-excludes-cached`（Claude：总量 = input + cache_creation + cache_read + output）。
   `cachedInputTokens` 是子集标记，不是加数。
5. **只读。** 读取不登录、不登出、不兑换重置额度、不充值、不发邮件、不注入斜杠命令；不抓
   cookie 或浏览器会话，不复制凭据。读取进程由本应用创建，读后由本应用关闭。
6. **只扫描用户配置的根。** 本地转录读取只走 S03 已启用（`enabled=1`）的只读来源根，
   没有隐藏默认根、没有全盘遍历；只解析数值，不落正文、不进搜索索引。
7. **漏读就不是总量。** 转录扫描的每一种明确遗漏——超过单文件上限、深度剪枝、含 `usage` 的行
   无法解析、文件/目录/条目不可读、文件数或字节预算用尽——都把整趟标记为**不完整**：窗口与累计值
   返回 `None`（界面显示「—」），并逐条列出原因。上限不放大，也不从漏读子集拼一个「看起来完整」的数。
8. **只有 assistant 记录是提供方样本。** `type` 不是 `assistant` 的记录（user、summary、system 等）
   即使内部有叫 `usage` 的对象也不计入。`usage` 里缺失、`null`、非整数的分量是**未知**，不是 0；
   一个窗口里只要有一条记录缺某个分量，该分量与总量的和就是未知；提供方真实报告的 `0` 保留为 0。
   一整趟完整扫描里某窗口确实没有任何记录，才是「真实的 0」。
9. **状态优先级。** Codex 一次读取的状态从重到轻：`unauthenticated` → `rate-limited`（即使另一个
   方法成功也保留）→ `error`（所有读取方法都失败且没有数值）→ `partial`（部分方法失败，或提供方
   返回空值）→ `ok`。每个失败方法单独保留 `method` / `kind` / `message` / `rpcCode` /
   `retryAfterMs`，写入快照 `error.details.methods`，不压成一条自由文本。

## 2. 对象

| 对象 | 关键字段 | 说明 |
|---|---|---|
| `AccountProfile` | `id`、`agentId`、`identityStatus`、`identityFingerprint`、`label`、`plan`、`source`、`observedAt` | 身份来自提供方自身（Codex `account/read`）。`confirmed` 必须有 fingerprint；邮箱只保留掩码（`a***@example.com`），fingerprint 是 SHA-256 |
| `QuotaPool` | `poolKey`、`identityStatus`、`accountProfileId`、`installationCount`、`label` | `pool_key` 唯一；`confirmed` 池必须引用账户行（数据库 CHECK） |
| `UsageSnapshot` | `status`、`freshness`、`sourceKind`、`source`、`capturedAt`、`expiresAt`、`error`、`retryAfterMs`、`nextAttemptAt`、`attempts`、`samples`、`unavailableFields` | 一次读取的结果。失败时 `samples` 为空、`error` 为真实 `AppError`，界面另列「上次成功读数」 |
| `UsageSample` | `metric`、`unit`、`window`、`label`、`usedPercent`、`remainingPercent`、`amount`、`totalTokens`、`inputTokens`、`cachedInputTokens`、`cacheCreationTokens`、`outputTokens`、`tokenSemantics`、`resetsAt`、`windowMinutes`、`estimated`、`source` | 一个指标一行；两个窗口是两个样本，从不求和 |

`window` 只能由提供方报告的分钟数推导（240–360 → `short`，1380–1500 → `daily`，
9000–11000 → `weekly`，38000–45000 → `monthly`）；未报告或落在区间外是 `unspecified`，
同时保留 `windowMinutes`，不猜。

## 3. 来源与新鲜度

| 产品 | 额度 | Token | 来源 | 不可读项 |
|---|---|---|---|---|
| Codex | `machine` | `machine` | `codex app-server`：`account/read`、`account/rateLimits/read`、`account/usage/read` | 未在认证范围内时 `unauthenticated` |
| Claude Code | `none` | `transcript` | 已启用历史根下的 `*.jsonl` 中 `message.usage` | 余额不可读；`/cost` 需用户手动执行 |
| CodeBuddy Code | `none` | `none` | — | 额度与 token 均不可机器读取；`/cost` 手动 |
| Gemini CLI | `none` | `none` | — | `/stats model` 手动，未验证机器出口 |
| OpenCode | `none` | `none` | — | 无官方用量命令 |
| Qwen Code | `none` | `none` | — | `/stats` 手动，未验证机器出口 |
| GitHub Copilot CLI | `none` | `none` | — | 无本地额度端点；IDE `copilot-shim` 不算独立安装 |
| Kimi Code | `none` | `none` | — | 旧 `kimi-cli` 同名，不继承能力 |
| Aider | `none` | `none` | — | `/tokens` 手动，未验证机器出口 |
| TRAE CLI | `none` | `none` | — | 仅本机观测，无官方用量接口 |

新鲜度在**读取时**由后端计算一次：`age = now - capturedAt`；`age < 0`（时钟跳变）记 `unknown`，
`age <= ttl` 记 `fresh`，否则 `stale`。TTL 默认 5 分钟，写入 `expiresAt`。

界面还会用**本地时钟**持续重算（每 15 秒，并在越过 `expiresAt` 时立刻生效）：页面一直开着也会从
`fresh` 变成 `stale`，徽标转 amber 并显示「已过期」；系统时钟大幅回拨时记 `unknown`。这个重算只降级、
不升级，也不发起任何读取。

**退避**：仅 `error` 与 `rate-limited` 记退避，`backoff = min(60s × 2^(attempts-1), 30min)`；
提供方给了 `retryAfterMs` 就用它（钳到 5s–1h）。退避写进 `usage_provider_state`，重启后仍然有效。
退避期内普通刷新返回 `skipped=true` 并给出 `retryAfterMs`，不重复启动 CLI；只有用户显式点
「强制刷新（忽略退避）」才传 `force=true`。`unauthenticated` / `unsupported` 或「诚实但没有数值」
的 `partial` 不设、也不清除退避，允许用户随时重试。

**一个方法的成功不能覆盖另一个方法的失败**：quota 读回 429、token 读成功时，状态仍是
`rate-limited`、`retryAfterMs` 保留、退避照常写入，同时保留已读到的 token 样本并在
`unavailableFields` 中列明 quota 缺失。

**缓存**：快照按池保留最新 40 条；失败读数的数字为空，旧数字只能从「上次成功读数」
（`last_ok_snapshot`）取，且始终带旧时间戳，不与当前状态混排。

## 4. IPC

| 命令 | 输入 | 输出 |
|---|---|---|
| `multicli_usage_overview` | — | `UsageOverviewView`：`accounts[]`（账户 + 池 + 快照 + `lastGood`）、`unconfirmedPools[]`、`catalog[]`、`lastAttemptAt`、`lastSuccessAt`、`note` |
| `multicli_usage_refresh` | `{agentId?, force?}` | `{overview, outcomes[]}`；`outcomes` 含 `status`、`skipped`、`reason`、`retryAfterMs` |
| `multicli_custom_cli_list` | — | `CustomCliView[]` |
| `multicli_custom_cli_create` | `{label, executable, args[], cwd?, productHint?}` | `CustomCliView` |
| `multicli_custom_cli_delete` | `{id}` | `()` |

所有失败返回真实 `AppError`。`multicli_usage_refresh` 默认遍历全部 provider，带 `agentId`
时只处理该产品；单个 provider 失败不会中止其余产品（它自己的 `outcome` 带错误原因）。未安装的产品
（`installed=false`，按发现注册表的 `installed` 状态判定，shim/残留不算）返回
`skipped=true / unsupported`，**不创建空额度池**。两个命令都是异步命令，读取期间不阻塞窗口主线程。

`force=false` 是普通刷新，遵守后端退避；`force=true` 只由界面上单独标注的强制动作传入，
普通按钮与错误重试都不使用它。

## 5. 自定义 CLI

`CustomCliView` 是「可执行文件 + argv 数组 + 工作目录」的本地预设，复用 S04 参数链
（`.exe` → MSVC quoting；`.cmd` → 转义 trampoline；`.ps1` → 单引号字面量 + pwsh 7.3+）。
创建时拒绝：

- 相对路径、不存在、目录、无实现链的扩展名；
- `cmd.exe` / `powershell.exe` / `pwsh.exe` / `wscript.exe` / `cscript.exe` / `mshta.exe` /
  `rundll32.exe` / `regsvr32.exe` / `bash.exe` / `wsl.exe` / `sh.exe` / `conhost.exe` / `wt.exe`
  ——它们的参数会再次变成命令行；
- `.cmd` 目标中含换行或 `"\` 的参数（S04 已在启动时拒绝，这里在创建时提前拒绝）；
  解释器黑名单按**文件名**判定，因此改名后的 `cmd.exe` 副本不会被识别为 shell。
  它的参数仍然是 argv，不会经过 shell 解析，但这一点只保证"不拼命令行"，不保证
  "任意可执行文件都安全"——这是用户自己的本地导入；
- 超过 64 个预设参数、单个参数超过 4096 字符、含 NUL。

预设参数排在用户参数**之前**，两者一起走同一套转义；合计超过 320 条在创建终端时拒绝。
`historyAccess` 与 `quotaAccess` 恒为 `unsupported`，`verification` 为 `user-local-import`。
删除只删配置，不停止已由它启动的终端。

## 6. 数据库（schema v7）

新增 `usage_accounts`、`usage_pools`、`usage_snapshots`、`usage_samples`、
`usage_provider_state`、`custom_clis`；不改动任何既有表。迁移为纯增量，不改 S01–S06 的
行 id 或外键；开迁前仍按既有机制做 `VACUUM INTO` 备份，事务失败回滚到 v6。数据库层用
CHECK 固化三条规则：`confirmed` 账户必须有 fingerprint；`pool_key` 唯一；`usage_samples`
的 `estimated` 只有 0/1 且百分比限制在 0–100。

## 7. 转发器识别（R1）

`.cmd`/`.bat`/`.ps1` shim 被当作**数据**读取（≤64 KiB），只展开 shim 自身目录变量。识别规则：

- 按**引号**分词，目录含空格时路径仍是同一个 token（不按空白拆路径）；
- 只认**命令位**：`&` / `call` 前缀之后、语句的第一个 token，且必须以 `.exe`/`.com` 结尾、
  canonicalize 后仍在 shim 同目录、确实存在；
- `echo`、`Write-Output`、赋值、注释、标签、以及作为其他命令参数的路径都**不是**执行；
- 不执行 shim、不做脚本求值、不拼命令行。识别不出就回落到 Node shim 解析，不猜。

## 8. 未覆盖

- 真实「离线」与真实 429 由代码路径与合成反例覆盖，未在本机触发真实网络断线。
- Codex 多 bucket（`rateLimitsByLimitId`）按 schema 支持，本机只观察到一个 `codex` bucket。
- Claude 转录的 token 语义来自本机真实记录；CodeBuddy 转录未被证实含 usage，因此不读。
- R1 未取得：用量页与官方字段的**实窗**目视对照、真实 429/断网、真实自定义 CLI 表单全链；
  本机仍只有 TRAE 一款扩展 CLI，第二个扩展产品的真实发现 + PTY 仍未完成。

## S06主管收尾边界

测量成功必须至少存在一个实际数值（百分比/金额/token分量），仅窗口长度/来源/全null样本不算成功，不清退避、不写lastSuccess；无值/partial不会把未知全局quota能力升级。日期未知的转录不能给今日/7天/30天的已测0，按已知分量保留累计并显示日期缺口。UI普通刷新遵守退避，skipped文案不猜原因；真实原因由outcome.reason提供。
