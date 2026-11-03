# 结构化运行时契约 runtime-v1（S05）

本契约是 S05 新增的**增量**契约，不修改 S00 冻结的 `domain.schema.json`、`events.schema.json`、`error.schema.json`。
一个结构化 Run 仍然是一条冻结的 `Run` 记录；本契约只描述协议层新增的请求、视图与事件 payload。

- 版本：`RUNTIME_SCHEMA_VERSION = 1`，出现在每个视图的 `schemaVersion`。
- 线上字段名：camelCase，未知字段一律拒绝（`deny_unknown_fields`）。
- 唯一 IPC 注册点仍是 `src-tauri/src/multicli_bootstrap.rs`；前端入口 `src/main.tsx → src/multicli/App.tsx`。
- 本契约不新增数据库表。Run/Session/Process 复用 S01 冻结模型；S05 验收时库为 **v6**，
  S06 起整体 schema 为 **v7**（新增 usage/自定义 CLI 表，S05 的 Run 语义不变）。

## 1. 三款产品的入口与传输

| 产品 | agentId | 传输 | 入口 argv（固定，不含用户文本） |
|---|---|---|---|
| Codex CLI | `codex` | `native` | `codex.exe app-server` |
| Claude Code | `claude` | `native` | `claude.exe -p --output-format stream-json --input-format stream-json --verbose --include-partial-messages --permission-mode default --permission-prompt-tool stdio (--session-id <uuid> \| --resume <id>)` |
| CodeBuddy Code | `codebuddy` | `acp` | `codebuddy.cmd --acp --acp-transport stdio` |

`--permission-prompt-tool stdio` 在 2.1.153 的 `--help` 中**不显示**，但被接受，且是唯一能拿到可应答权限请求的通道（见 §6.2）。

启动链复用 S04 已验证的 `.exe` / `.cmd` / `.ps1` 三条链（`terminal::launch::plan_launch`），运行时不自己拼 shell 字符串。

### 1.1 进程级环境差量

| 产品 | 设置 | 删除 | 原因 |
|---|---|---|---|
| Codex | — | — | 无 |
| Claude Code | — | — | 无 |
| CodeBuddy Code | `FORCE_HEADLESS=0`、`LITE=0` | `SERVER__PORT`、`SERVER__HOST`、`CODEBUDDY_FORCE_HEADLESS_BUNDLE`、`CODEBUDDY_FORCE_LITE_WB_BUNDLE`、`CODEBUDDY_MCP_CONFIG` | 继承宿主的 `SERVER__PORT` 会让 ACP 子进程立刻以 `EADDRINUSE` 退出且**零 stdout**；bundle 变量会强制走 headless |

只作用于本应用创建的子进程，不改全局环境、不改 CLI 配置文件。

## 2. 身份与所有权

- Run 由用户动作创建："新建运行"或"继续历史会话"。**索引/详情/能力表打开不发模型请求。**
- 一个 Run 拥有恰好一个 CLI 进程与一个 `streamId = "runtime/<runId>"`。
- 同一 Run 内的后续消息**不新建 Run**；继续历史 Session 一定**新建 Run**、沿用原 Session 与 `nativeId`。
- 进程身份为 `hostId + pid + creationTime`，在 spawn 后立刻捕获；结束前复核，不复用 PID。
- 子进程加入 kill-on-close Job；Job 分配失败直接放弃启动，不允许无约束进程树。
- 外部 CLI 不在可寻址范围内：本模块没有"按 PID 接管"的入口。

## 3. IPC 命令

| 命令 | 入参 | 返回 | 说明 |
|---|---|---|---|
| `multicli_runtime_capabilities` | — | `RuntimeCapabilitiesView` | 逐产品逐字段的能力与证据层 |
| `multicli_runtime_start` | `{spec: RuntimeStartRequest}` | `RuntimeRunView` | 建连接；`prompt` 存在时发首条消息 |
| `multicli_runtime_resume` | `{spec: RuntimeResumeRequest}` | `RuntimeRunView` | 继续历史 Session，新 Run |
| `multicli_runtime_send` | `{query: {runId, text}}` | `RuntimeRunView` | 同一连接内追加一轮 |
| `multicli_runtime_cancel` | `{query: {runId, graceMs?}}` | `RuntimeRunView` | 取消当前 turn，不杀进程 |
| `multicli_runtime_answer` | `{query: {runId, permissionId, optionId}}` | `RuntimeRunView` | 只接受该请求实际给出的选项 |
| `multicli_runtime_stop` | `{query: {runId, graceMs?}}` | `RuntimeRunView` | 结束自有进程树 |
| `multicli_runtime_get` | `{runId}` | `RuntimeRunView` | 视图即重同步源 |
| `multicli_runtime_read` | `{query: {runId, cursor?, maxEvents?}}` | `RuntimeEventPage` | 只读增量事件 |
| `multicli_runtime_list` | — | `RuntimeListView` | 管理表 |

上限：`MAX_RUNS = 8`；`prompt ≤ 256 KiB`；后续消息 `≤ 64 KiB`；`maxEvents ≤ 512`（默认 256）；
权限等待默认 300 s（可配 5 s–3600 s）。

### 3.1 请求校验

`RuntimeStartRequest`：`installationId` 非空、`cwd` 为存在目录的绝对路径、文本无 NUL 且在长度上限内。
`RuntimeAnswerRequest.optionId` 必须与该请求 `options[].optionId` **逐字相等**，否则 `invalid_argument`，且**不消耗**该请求。
`RuntimeResumeRequest`：`sessionId` 必须存在、必须已是该产品 Session、必须有 `nativeId`。

## 4. 状态机

### 4.1 Run 执行状态（复用冻结 `ExecutionStatus`）

`starting → running → stopping → exited|failed`，任意中间态可落到 `unknown`（附原因）。
`exited` / `failed` 是终态，晚到事件不得重开（数据库层 `set_run_state` 亦有同样约束）。

### 4.2 交互状态（复用冻结 `InteractionStatus`）

- `awaiting-permission` 只由**真实 pending 权限请求**产生；
- `working` 只由协议消息（turn 开始、正文/推理增量、工具事件）产生；
- `idle` 只在 turn 正常结束且无 pending 请求时成立；
- 其余一律 `unknown`。**静默不推断 idle。**

### 4.3 Turn

`RuntimeTurnView.state`：`running | completed | cancelled | failed`。

- **回合自用户的发送动作开始**（Claude Code 不发"回合开始"帧）。此时 `turnId` 是**临时 id**（`turn-<uuid>`）；
  adapter 给出真实原生 id 时，该 id 被**附着到当前回合**，不新建回合。
- **已结束的回合 id 会被记入 retired**。一个 `TurnCompleted` 只有满足以下之一才被应用：
  它是当前回合的 id，或当前回合仍是临时 id（原生 id 尚未到达）。
  **其余一律忽略**并记一条备注——同一连接上旧回合可能晚于新回合结束，仅凭"当前回合在运行"就接受它会
  把新回合改名并关闭。
- 新消息会**显式退休**上一个回合（含"已请求取消但未确认"的回合），因此旧回合的晚到事件不会影响新回合。
- 取消的确认语义见 §4.6。

### 4.4 工具：双账本三字段

这是本阶段最重要的模型修正。**"没有权限请求"不等于"被拒绝"**：Claude Code 在默认模式下对读操作与项目内编辑**静默执行**，从不询问。
因此每个工具携带三个互不折叠的字段：

| 字段 | 取值 | 含义 |
|---|---|---|
| `authorization` | `not-requested` / `pending` / `allowed` / `allowed-always` / `denied` / `auto-denied` | 用户做了什么决定，或**根本没人被问** |
| `execution` | `unknown` / `running` / `executed` / `not-executed` | 工具是否真的跑了 |

`execution` 的判定必须区分「没有执行证据」和「执行失败」：`failed` 只有在**带回结果体**时才记为
`executed`+`outcome=failed`；没有结果体时记 `unknown`。把 `failed` 直接写成 `not-executed` 会告诉用户
"什么都没发生"，而命令可能已经执行过并改动了磁盘。
| `outcome` | `unknown` / `ok` / `failed` | 跑了的那个是否成功 |

`quadrant()` 由三者派生，覆盖完整 2×3：

| 授权 | 执行 | 结果 | quadrant |
|---|---|---|---|
| allowed | executed | ok | `allowed-executed-ok` |
| allowed | executed | failed | `allowed-executed-failed` |
| allowed | not-executed | — | `allowed-not-executed` |
| denied | any | any | `denied-not-executed` |
| not-requested | executed | ok | `never-asked-executed-ok` |
| not-requested | executed | failed | `never-asked-executed-failed` |
| not-requested | not-executed | — | `never-asked-not-executed` |
| 其它 | — | — | `unknown` |

最后三行是"静默"区：**用户从未被咨询**。`never-asked-executed-failed` 是风险最高的一格，必须能在 UI 上一眼看出。

`is_error` 保留为**协议原文回显**，永不参与判断：

- Claude 2.1.153 全量 `tool_result`（n=15）实测：成功侧按工具确定性地不同（`Write` 3/3 不带该字段，`Bash` 1/1 显式 `false`）；失败侧同一个 `true` 覆盖五种无关成因（用户拒绝、客户端契约错误、自动拒绝、hook 拒绝、普通执行失败）。
- 因此"缺省 = `false`"的推断会让真实失败显示成成功。

轮次结束时仍处于 `running` 的工具，落为 `execution = unknown` 并计入 `notes`（"N 个工具调用在本轮结束时没有结果"）。这消灭了"账本永久未完结"。

工具与权限**按 `toolCallId` 双向链接**，不按到达顺序：请求可能先到（Claude：先 `control_request` 后 `tool_use` 结果），也可能后到（Codex：先 `item/started` 后 `item/*/requestApproval`）。`toolCallId` 对三款都是不透明字符串：Codex 用 `itemId`，Claude 用 `tool_use.id`，ACP 用 `toolCallId`。

### 4.5 权限请求

`RuntimePermissionView.state`：`pending | answered | expired | cancelled`。

- 创建即 `pending`，`permissionId` 由本应用生成，`nativeRequestId` 保留协议原 id；
- **归属门**：请求到达时先判断它是否还属于可授权的操作——回合正在取消（`cancel_pending`）、所属回合已结束/已退休、
  或携带的本机 `turnId` 与当前回合不符时，请求**直接记为 `cancelled`**（`autoDenied = true`），
  并向对端回送一次拒绝。取消中的旧操作不能借迟到请求重新获得授权；没有 `turnId` 的协议按"当前回合是否仍可授权"判断。
  残余窗口：取消后**立即**开新回合时，无 `turnId` 的协议无法区分旧请求与新请求，当前按"新回合已开始即可授权"处理；
  彻底消除需要引擎在权限请求里携带回合标识。
- 只有 `pending`、未过期、且 `answerable` 才能应答；状态不符返回 `conflict` 并附当前状态；
- **应答入口自己检查真实 deadline**（不依赖清扫线程的下一次 tick）：已过期则走**同一条一次性收敛路径**——
  标记 `expired`、`autoDenied = true`，并**仍向 CLI 回送一次该请求自己的 deny 选项**，返回 `timeout`。
  到期检查与清扫共用该路径，因此"过期 50 ms 但还没被 sweep"既不会批准，也不会漏掉对端的拒绝义务。
- 一次应答只允许一个终局，失败必须区分清楚：

  | 写入结果 | 状态 | 可否重试 |
  |---|---|---|
  | 明确未写出任何字节（可重试 Io） | 保持 `pending`，`reason` 说明"未发送成功，可重试" | 可重试 |
  | 写入结果未知 | 保持 `pending`，`reason` 说明结果未知 | **不可**盲目重发（可能重复批准），提示取消或结束 |
  | 已写出但对端未应用（如 ACP `{resolved:false,queued:true}`） | 保持 `pending`，`reason` 说明"已提交未应用"，不能当成已授权/执行 | 由用户显式决定重试或取消，客户端不自动重投 |
  | 应答已送出且未报告未应用 | `answered`，记录用户决定；不表示工具已执行 | — |

- **取消与应答的顺序**：`cancel` 置 `cancel_pending` 后，**有界等待在途应答落地**再返回；
  取消已生效后，后到的成功回写**不得覆盖** `cancelled`/终态，只写备注。先前已提交的允许排在取消之前，
  取消不回滚它，但也不会再提交第二次。
- 应答后立刻记录 `selectedOptionId` 与 `resolvedAt`——**这是"用户拒绝"的唯一权威判据**（出站账本），不依赖任何回执，
  也不从工具输出文本或文档正文推断；
- 超时：用该请求自己的 deny 选项回送，记为 `expired` + `autoDenied = true`，**永不自动允许**；
- turn 结束：该 turn 下所有 `pending` 变 `cancelled`，之后再批准一律拒绝；
- 连接断开/新 Run：旧 Run 的请求不可达（按 Run 隔离），晚到应答在 `answer` 入口即被拒。
- 应答在线路锁内重新核对期限及运行/取消状态，排队等待不延长期限；退出会作废旧权限。确认回合已结束后清除其旧拒绝义务，避免重复请求已被对端清理的id。
- 用户消息与出站决定各写入领域库，不依赖CLI回显；明确零字节发送失败结束失败回合并允许重试，结果未知保持未知，不假装空闲。应答IPC在后台线程等待，不阻塞窗口。

### 4.6 取消：发出 ≠ 已确认

`cancel` 只负责**发出**中断请求，并把 `interactionState` 置为 `unknown`。回合**不会**因为发出了请求就变成
`cancelled`：状态只在收到对端的 `TurnCompleted` 时改变。

| 情况 | 结果 |
|---|---|
| 对端在宽限期内确认 | 回合 `cancelled`，`interactionState` 收敛为 `idle`（仍有 pending 请求则 `awaiting-permission`），该 id 退休 |
| 宽限期超时 | 回合保持 `running`，`interactionState = unknown`，写备注；**不伪造 idle** |
| 取消请求发送失败 | 立即返回错误，`interactionState = unknown`，并提示该回合并未停止 |

未确认时 `cancel_pending` 保持为真：用户仍可发送新消息（显式继续），此时旧回合被退休并记一条备注；
UI 侧在 `unknown` 状态下禁用发送，避免"无意同时写入仍在执行的旧回合"。

## 5. 事件流

事件沿用冻结 `EventEnvelope`。`seq` 只在 `(runId, streamId)` 内单调；`nativeEventId` 或稳定 `sourceKey` 去重；
未知 kind 以 `protocol/unknown` 保留（`executed: false`）并按 method 计数，供报告解释。

| kind | payload 关键字段 |
|---|---|
| `run/state` | `executionState`、`interactionState`、`nativeSessionId?`、`exitCode?`、`reason?` |
| `turn/started` | `turnId`、`model?`、`effort?` |
| `turn/completed` | `turnId`、`status`、`reason?` |
| `message/delta` | `messageId`、`role`、`index`（字符偏移）、`text` |
| `message/completed` | `messageId`、`role`、`text`（权威全文，替换累积值） |
| `reasoning/delta` | `messageId`、`index`、`text` |
| `tool/call` | `toolCallId`、`permissionId?`、`name`、`kind`、`input`、`status` |
| `tool/update` | `toolCallId`、`status`、`output?`、`isError` |
| `permission/requested` | `RuntimePermissionView` 全文 |
| `permission/resolved` | `RuntimePermissionView` 全文（含 `selectedOptionId`/`state`/`autoDenied`） |
| `warning` | `code`、`message` |
| `protocol/unknown` | `method`、`sample`、`executed:false` |

### 5.1 增量与去重

- 增量按**字符偏移**合并：等于当前长度则追加；小于则只取真正新增的尾部（重投不重复）；大于则追加并记 `warning`，同时在 `notes` 写明缺口。
- `message/completed` 携带权威全文并**替换**累积文本，因此丢失/乱序的增量不会污染最终正文。
- 日志上限：每 Run 4000 事件 / 4 MiB；被淘汰时 `droppedBefore` 前移且 `journalTruncated = true`。
  客户端游标低于 `droppedBefore` 时必须以 `runtime_get` 的视图重同步。

### 5.2 通知

`multicli://runtime` 只推 `{runId, seq, kind, interactionState, executionState}`，**不含正文**。
前端收到后按 `runId` 拉 `runtime_get`，80 ms 去抖。事件不走向 WebView 的无界队列。

## 6. 三款协议实测事实

### 6.1 Codex app-server（0.166.0）

```
initialize {clientInfo, capabilities:{experimentalApi:true}} → result{userAgent, codexHome, platformFamily}
initialized                                  (无 params 的通知)
thread/start {cwd, approvalPolicy:"on-request", sandbox:"read-only"|"workspace-write", model?, config?}
     → result.thread.id / result.thread.sessionId
thread/resume {threadId, cwd, approvalPolicy, sandbox}
turn/start  {threadId, input:[{type:"text",text}]} → result.turn.id
turn/interrupt {threadId, turnId}
```

服务端 → 客户端（实测 + schema 双向确认）：

| method | 语义 |
|---|---|
| `item/agentMessage/delta` `{itemId, delta, turnId}` | 正文增量 |
| `item/reasoning/textDelta`、`item/reasoning/summaryTextDelta` | 推理增量 |
| `item/started` / `item/completed` `{item:{type,...}}` | 工具/消息生命周期 |
| `item/commandExecution/requestApproval` `{itemId, turnId, command, cwd, reason, ...}` | 命令审批 |
| `item/fileChange/requestApproval` `{itemId, turnId, reason, grantRoot, ...}` | 写文件审批 |
| `turn/started` / `turn/completed` `{turn{id,status,error}}` | 轮次 |
| `error` `{error{message,codexErrorInfo}, willRetry}` | 服务端错误 |

审批应答统一为 `{"decision": ...}`，取值按 Surface 不同：

| Surface | allow | allow-always | deny | deny+cancel |
|---|---|---|---|---|
| `item/commandExecution/requestApproval` / `item/fileChange/requestApproval` | `accept` | `acceptForSession` | `decline` | `cancel` |
| 旧 `execCommandApproval` / `applyPatchApproval` | `approved` | — | `denied` | `abort` |

`decline` 结束**该次工具调用**但让回合继续；`cancel` 会**中断整个回合**。两者在 UI 上是两个独立按钮，绝不合并。
`itemId` 会跨 `item/started`、审批请求、`item/completed` 稳定复用，因此账本按 `itemId` 做 upsert。

`item.createdAt` 缺失；完成事件里的 `itemsView:"notLoaded"` 表示这不是权威全量。`item/completed` 自带 `aggregatedOutput`，是比增量缓冲更可靠的来源；中途 `item/commandExecution/outputDelta` 不参与账本（避免覆盖丢增量）。

### 6.2 Claude Code（2.1.153）

```
claude -p --output-format stream-json --input-format stream-json --verbose
       --include-partial-messages --permission-mode default
       --permission-prompt-tool stdio (--session-id <uuid> | --resume <id>)
```

- **一个进程服务多轮**：`result` 之后进程仍等待 stdin；关闭 stdin 才退出（实测两轮同 session，exit 0）。
- `--session-id` 必须是全新 UUID，重复会报 "already in use"；回到既有会话只能用 `--resume`。
- 输入：`{"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]}}`
- 输出类型：`system/init`（含 `session_id`、`tools`）、`assistant`（content blocks：`thinking`/`text`/`tool_use`）、
  `user`（`tool_result{tool_use_id, content, is_error}`）、`stream_event`（`message_start`/`content_block_delta`/`message_stop`）、
  `control_request`、`control_response`、`result`。
- 推理文本在 `assistant.content[].thinking`（**不是** `reasoning_content`），跨块靠 `message.id` 归并。
- **权限往返**（本条未在 `--help` 中体现，但实测可用）：

  ```
  ← {"type":"control_request","request_id":"<uuid>","request":{
       "subtype":"can_use_tool","tool_name":"Write","input":{...},
       "description":"...","permission_suggestions":[...],"tool_use_id":"call_..."}}
  → {"type":"control_response","response":{"subtype":"success","request_id":"<uuid>",
       "response":{"behavior":"allow","updatedInput":{...}}}}          // 允许
  → {"type":"control_response","response":{"subtype":"success","request_id":"<uuid>",
       "response":{"behavior":"deny","message":"...","interrupt":false}}}   // 拒绝
  ```

  `allow` **必须**带 `updatedInput`（对象）。只发 `{"behavior":"allow"}` 会被 CLI 的 schema 拒绝，
  该错误以普通 `tool_result`（前缀 `Tool permission request failed`）回给模型，模型于是**换工具重试**：
  实测一次坏帧产生 5 次额外请求、成本翻倍，而 `result` 仍是 `subtype=success`、`is_error=false`。
  工具正文可合法包含同一前缀，因此不能据该字符串熔断；协议错误须来自结构化错误帧。
- 拒绝消息中的 `[MCLI:denied]` 只是出站文案，不是不可伪造证据。拒绝判定来自关联请求的出站决定账本，不能从工具/文件正文推断。
  **不使用** `result.permission_denials[]`：它把契约错误与自动拒绝也计入，会污染统计。
- `updatedInput` 传改后的值是否被采纳 → **未验证**，因此产品不提供"改后放行"，`updatedInput` 原样回传。
- 取消：客户端发 `{"type":"control_request","request_id":"int-...","request":{"subtype":"interrupt"}}`，
  收到 `control_response` 后回合以 `result.subtype = error_during_execution` 结束（实测）。

### 6.3 CodeBuddy ACP（当前实测2.179.3，保留2.179.0历史观察）

```
initialize {protocolVersion:1, clientCapabilities:{fs:{readTextFile,writeTextFile}, terminal:false}}
   → {protocolVersion:1, agentCapabilities:{loadSession:true, ...}, authMethods:[...]}
session/new {cwd, mcpServers:[]}            → {sessionId, models, modes, configOptions, multitaskOn}
session/load    {sessionId, cwd, mcpServers:[]}   // cwd 与 mcpServers 均为必填
session/resume  {sessionId, cwd, mcpServers:[]}
session/prompt  {sessionId, prompt:[{type:"text",text}]}   // 应答在回合结束时到达
session/cancel  {sessionId}                                 // 通知
```

- `initialize` 的 `result` 只有三个顶层字段：`protocolVersion`（**数字** 1）/ `agentCapabilities` / `authMethods`；
  **没有** `serverInfo`、`agentInfo`、`connectionId`。
- 只传 `{sessionId}` 会被 `-32602` 拒绝（`cwd`、`mcpServers` 必填）。只按协商能力选择方法，错误不触发猜测方法或降级重试。
- `session/prompt` 的失败是 `result.stopReason = "refusal"` + `result._meta["codebuddy.ai/errorMessage"]`（内嵌 JSON 字符串），
  **不是** JSON-RPC error。
- 权限有两条声明面：ACP 标准 `session/request_permission`（应答 `{outcome:{outcome:"selected",optionId}}`），
  以及产品自定义 `interruption_request` 更新 + `_codebuddy.ai/resolveInterruption {sessionId,toolCallId,decision}`。
  标准面已实测Reject后文件不存在、Allow后另一个文件生成。专有面applied/pending/rejected仅经合成对端覆盖，不能据标准面通过宣称专有面也完成真实样本。

  登记生命周期：请求到达时**写入 adapter 的待答表**（UI id、wire 原生 id、实际选项、到达时间），
  应答时先校验 `optionId` 属于该请求**再**取走，非法选项不消耗请求；线写失败则放回表中以便重试。
  取消、关闭、以及超过 30 分钟的陈旧项会被清理。
- **只调用已协商的方法**：`initialize` 只声明 `loadSession` 时不发送 `session/resume`；
  `session/resume` 仅在 `agentCapabilities.resumeSession === true` 时使用，不做 `-32601` 试探式回退。
  `load` 用于展示既有会话，`resume` 用于继续执行，两者语义不混。

- 当前版本声明loadSession而没有resume扩展：继续执行使用`session/load`建立原上下文，再显式`session/prompt`；只加载不算恢复执行成功。
- 2.179.3的Reject会结束当前轮次，随后可显式发下一条消息；不能承诺与Claude的deny完全相同。
- ACP正文分片没有offset：适配器按字符数累加偏移，每次prompt产生独立消息id，完成时发权威全文并落库。中文/emoji与重复片段不可丢；load阶段的历史回放不是新Run事件，不追加为新消息或新工具记录。
- prompt请求id在写入前登记，避免立即回复早于路由登记；缺失策略选项或未应用决定时，显式探针立即停止该阶段，不重复提交或无限积累错误。

## 6.4 能力表的三个轴

`RuntimeCapabilityView` 把三件事**分开**，任何一轴都不能顶替另一轴：

| 轴 | 取值 | 含义 |
|---|---|---|
| 就绪 | 内部判定 | 安装是否有可用 canonical 可执行文件（不可用即 `unsupported`） |
| 能力状态 | `supported` / `unsupported` / `unverified` | 本阶段是否在该版本上复现过该能力 |
| 证据层 | `live` / `document` / `fixture` | 证据来自真实 CLI、文档还是合成夹具 |

只有**实测过的那个版本**（Codex `0.166.0`、Claude `2.1.153`、CodeBuddy `2.179.3`）才可能给出
`supported`；其他版本（含未知版本、空版本）**一律 `unverified`**，证据层为 `document`，
不继承已验证版本的能力。`live` 只表示"证据来自真实 CLI 观察"，不等于"已验证"。

## 7. 已知限制

1. **Codex 审批以"端点可达"为前提**：S05 初次验收时本机 `chatgpt.com` / `api.openai.com` 被 DNS 污染到
   `31.13.94.10` 且 443 不可达，任何真实回合都在约 200 s 后以 `turn/completed{status:"failed"}` 结束，
   审批应答一帧都没发出过。R2 期间网络恢复后，同一探针实测：拒绝 → 目标文件不存在；允许 → 文件生成且内容正确；
   `turn/interrupt` 被对端确认为 `cancelled`；`thread/resume` 在同一 native session 上开出新回合。
   **样本数为 1 次/项**，且"端点不可达"会以完全相同的方式表现为回合失败，因此不能把一次通过当成稳定性结论。
2. **CodeBuddy认证旧失败保留**：早期连接曾返回Authentication required；用户确认2.179.3普通CLI能回复后，主管新建ACP连接已取得真实权限、正文、取消与继续样本。没有调用authenticate、getUserInfo或读取/复制凭据；不能推断早期失败的根因或恢复原因。专有interruption面仍没有真实样本，旧失败与当前观察不得混为永久不支持或稳定性保证。
3. `updatedInput` 可改性未验证；`permission_denials[]` 不可作判据。
4. ACP stdio 仍会绑定本地 HTTP 端口并改写 `~/.codebuddy/web-ui-port.json`（CLI 自身行为，本应用不写该文件）。
5. 本阶段只有单 Run 内多轮；跨 Run 并发同一 Session 被拒绝（`conflict`），不做自动接管。
6. PTY 模式仍是 S04 的独立能力；结构化 Run 不是 PTY Run，二者不互换。

## 8. 非目标

- 不实现远程/网络 Run、不做账号抓取、不读凭据、不把 API key 写入数据库或 Git。
- 不实现"自动允许一切"或"静默拒绝一切"的策略；不用 `--dangerously-skip-permissions` 之类参数换取"能跑通"。
- 不推断未声明的协议方法：缺失能力返回 `unsupported` / `unverified`，不循环试方法。
- 不做额度和计费（S06）。
