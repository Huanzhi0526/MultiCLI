# MultiCLI 安装与外部进程发现契约 v1

适用0.1.0。安装识别与外部进程只读观察，复用domain/error/events schema；当前整体数据库v7，签名产品范围见DISCOVERY-SIGNATURES。仅local-windows，不读取凭据或提供接管接口。

## 1. 命令

| 命令 | 输入 | 输出 |
|---|---|---|
| `multicli_list_installations` | `query:{ agentId?, limit, cursor? }` | `Page<InstallationDiscoveryView>` |
| `multicli_list_processes` | `query:{ agentId?, workspaceId?, state?, limit, cursor? }` | `ProcessDiscoveryPage` |
| `multicli_refresh_discovery` | 无参数 | `ScanView` |
| `multicli_update_discovery_settings` | `query:{ intervalMs }` | `DiscoverySettings` |

`limit` 为 1..200，越界或畸形参数返回 `invalid_argument`。`Page<T>` 沿用冻结形状
`{ items, nextCursor, total }`；`total` 是**同一过滤条件**的计数。

`multicli_update_discovery_settings`持久化当前profile扫描间隔，写入`<dataRoot>/discovery-settings.json`，采用临时文件+rename，不修改CLI目录、系统代理或全局设置。baseline诊断命令保持开发用途。

## 2. `InstallationDiscoveryView`

| 字段 | 类型 | 说明 |
|---|---|---|
| `installationId` | `string \| null` | 稳定 id：由 host + agent + profile + 可执行身份派生；版本变化不产生新 id |
| `agentId` | `string` | 产品注册表中的id，范围见签名技术说明 |
| `product` | `string` | 展示名 |
| `signatureId` | `string` | 命中的签名 id（`codex-cli` 等） |
| `status` | `installed \| unverified \| missing-target \| historical-residue` | 见下 |
| `entryKind` | `native-exe \| node-package \| cmd-wrapper \| powershell-wrapper` | 入口形态 |
| `executable` | `HostPath \| null` | 用户实际运行的入口（原生 exe 或 shim） |
| `runtime` | `HostPath \| null` | Node 入口解析出的解释器；原生 exe 为 null |
| `entryScript` | `HostPath \| null` | Node 包入口脚本 |
| `version` | `string \| null` | **只**来自成功的 `--version` 探测；未成功一律 null，不填猜值 |
| `versionSource` | `probe \| none` | 版本来源；与探测时间分开 |
| `probe.status` | `ok \| failed \| timeout \| skipped \| not-attempted` | 探测状态 |
| `probe.reason` | `string \| null` | 固定、path-free 原因 |
| `probe.checkedAt` | `string \| null` | 上次探测时间（UTC，毫秒） |
| `probe.durationMs` | `number \| null` | 探测耗时 |
| `probe.arguments` | `string[]` | 实际探测参数（如 `["--version"]`） |
| `configProfileId` | `string` | 未取得真实 profile 时为 `unconfirmed-local` |
| `configProfileStatus` | `confirmed \| unconfirmed` | 本契约恒为 `unconfirmed`，不伪称 `default`/当前账号 |
| `issueCode` / `issueReason` | `string \| null` | `missing-target`、`unexpected-entry-script`、`historical-residue` 等 |
| `discoveredFrom` | `string` | `path` / `app-data-root` / `local-app-data-root` / `declared-residue` 等 |
| `discoveredAt` | `string` | 首次进入当前扫描集的时间 |
| `lastSeenScan` | `number` | 最近一次看到它的扫描代次 |

`status` 语义：

- `installed`：入口存在且可解析（原生 exe 可 canonicalize，或 shim 解析出的
  Node 入口存在、位于 shim 目录内、是声明的包入口）。**不要求**版本探测成功。
- `missing-target`：shim/exe 记录还在，但目标文件消失。
- `unverified`：找到同名入口但无法确认是目标产品（例如 shim 指向非声明入口）。
- `historical-residue`：签名声明的目录仍存在，但里面没有可运行入口。

目录存在不等于已安装：只有找到入口才会出现 `installed`。

## 3. `ProcessDiscoveryPage`

```json
{ "items": [ProcessDiscoveryView], "nextCursor": string|null, "total": number, "scan": ScanView }
```

### `ProcessDiscoveryView`

| 字段 | 类型 | 说明 |
|---|---|---|
| `identity` | `{hostId,pid,creationTime} \| null` | 只有 `hostId + pid + OS creationTime` 三者齐全才是身份 |
| `pid` | `number \| null` | 便于展示；**不是**身份 |
| `agentId` / `product` / `installationId` | 可空 | 识别结果；未识别为 null |
| `entryKind` | 可空 | 入口形态 |
| `recognition` | `confirmed \| likely \| null` | 识别强度；`likely` 只来自“可信目录 + 同名 exe” |
| `evidence` | `string[]` | `installation-exe` / `node-package-entry` / `path-entry+filename` |
| `state` | `alive \| exited \| unknown` | 见下 |
| `freshness` | `fresh \| stale` | 本代次是否更新过 |
| `stateReason` | `string \| null` | `process-ended` / `pid-reused` / `access-denied` / `not-observed` / `recognition-degraded` |
| `executable` | `HostPath \| null` | 镜像路径（可读时） |
| `redactedCommand` | `string \| null` | 只含入口名、声明过的结构性 flag 和 `<n withheld>` |
| `parentIdentity` | 身份 \| null | 仅在父身份与时间顺序都可验证时给出 |
| `parentNote` | `string \| null` | `parent-not-found` / `parent-unverified` / `parent-newer-than-child` / `identity-unconfirmed` |
| `sourceProgram` | `string \| null` | 父进程的程序名（仅名字） |
| `workspaceId` | `string \| null` | 仅在 CLI 带显式目录参数且该目录已登记为工作区时非空 |
| `workspaceEvidence` | `string \| null` | 目前只有 `explicit-directory-argument` |
| `sessionId` | `null` | **发现视图此字段为null**，见 §5 |
| `firstSeenAt` / `lastSeenAt` | `string` | UTC，毫秒 |
| `lastSeenScan` | `number` | 最近看到它的代次 |

`state` 语义：

- `alive`：本代次看到同一身份。
- `exited`：PID 消失（`process-ended`），或同一 PID 的创建时间被验证已变化
  （`pid-reused`）。退出条目保留有限代次后从视图收敛。
- `unknown`：PID 存在但创建时间不可读（权限不足等），或本代次未能重新识别。
  **不是**退出。

### `creationTime` 精度

`creationTime` 是 Windows `GetProcessTimes` 返回的创建时间，UTC ISO-8601，
**7 位小数（100 ns）**，例如 `2026-11-02T01:30:14.1234567Z`。禁止用观察时间、
序号或取整秒代替。取不到创建时间时该条目的 `identity` 为 null，只作为“未确认候选”
显示，不生成稳定身份、不绑定旧关联。

### `ScanView`

| 字段 | 说明 |
|---|---|
| `generation` | 扫描代次；每次扫描尝试 +1 |
| `status` | `idle \| scanning \| ok \| partial \| error` |
| `inFlight` | 是否有扫描在途 |
| `lastAttemptAt` / `lastSuccessAt` | 分开记录；整次失败只推进 attempt |
| `error` | 部分失败或整次失败的真实 `AppError`；无失败为 null |
| `installError` | 安装登记/持久化错误（若发生） |
| `intervalMs` | 当前扫描间隔 |
| `scope` | `local-windows` |
| `sampledProcesses` / `unreadableProcesses` / `trackedProcesses` | 本代次计数 |
| `note` | 固定说明：按间隔采样，短于间隔的进程可能漏见，不是审计日志 |

失败语义：

- 整次枚举失败：`status = partial`（已有成功历史时）或 `error`（从未成功），
  `error` 非空，上一条目全部标 `stale`，`state` 不变。
- 部分补查失败：`status = partial`，受影响条目 `unknown` + `stale`。
- 失败不返回空列表冒充成功。

## 4. 游标与代次

游标是 base64url 的 JSON `{sort,key,id,generation}`：

- `multicli_list_processes` 绑定 `ScanView.generation`。
- `multicli_list_installations` 绑定安装集 `epoch`（安装集身份/状态/版本变化时才 +1，
  因此重复扫描同一安装集不会让在途分页失效）。

跨代次继续分页返回 `conflict`，`details = { stage:"cursorGeneration",
cursorGeneration, currentGeneration }`。宁可让 UI 重新加载第一页，也不静默漏/重。
游标属于别的列表（`sort` 不匹配）或不是合法 base64/JSON 时返回 `invalid_argument`。

## 5. 会话关联

发现服务**不创建** Session / Run。原因：`Run.transport` 的冻结取值是
`native | acp | pty`，没有一个能准确表达“未连接的只读观察”；把观察写成其中任何一种
都是伪造协议。因此外部实例在独立发现视图中以 `sessionId = null` 呈现：

- 不声称接上真实聊天/历史；
- 不伪造 native/acp/pty 已连接；
- 后续真实关联阶段必须先有可核对的 native session id，再按身份（而非 PID）建立关联，
  不得把新旧观察混在一起。

## 6. 脱敏

完整命令行只存在于识别过程的内存中。离开进程前经过白名单：

- 只保留入口 basename（不含目录）、签名声明的结构性 flag、`<value>` 与
  `<n withheld>`；
- 提示词、key/token、URL（含 query）、账号、任意用户参数、路径一律丢弃；
- 数据库、IPC、错误 `details`、日志、诊断文件都只出现脱敏结果。

脱敏行为由discovery/redact.rs及对应合成测试维护。

## 7. 设置

`DiscoverySettings = { intervalMs, minIntervalMs:1000, maxIntervalMs:60000, note }`。
越界返回 `invalid_argument`。持久化位置固定 `<dataRoot>/discovery-settings.json`，
写入采用临时文件 + rename；不写任何 CLI 目录、注册表或系统代理设置。

## 8. 与会话导航的关系

- `multicli_list_workspaces / list_agents / list_sessions / get_session /
  list_capabilities / export_diagnostics` 行为不变。
- 发现会为真实产品登记 Agent 与安装行（`unconfirmed-local` profile），因此 Agent
  列表会显示出现过安装的产品；没有安装的产品不会凭空出现。
- 当前整体数据库schema为v7；发现表最初引入于v3；迁移前自动 `VACUUM INTO` 备份，事务化，失败整体回滚。
