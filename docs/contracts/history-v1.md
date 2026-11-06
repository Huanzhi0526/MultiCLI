# MultiCLI 只读历史契约 v1

适用0.1.0。定义只读索引、搜索和显式继续目标解析，复用domain/error/events及discovery契约。历史视图schemaVersion=1与整体数据库v7分开。时间为UTC ISO-8601毫秒，分页limit为1..200。历史源不提供编辑、写入或通用进程接管。

## 1. 命令

| 命令 | 输入 | 输出 |
|---|---|---|
| `multicli_list_history_sources` | `query:{ limit, cursor? }` | `Page<HistorySourceView>`（游标 JSON `{sort:"history-sources",key,id}`，编码为 base64url） |
| `multicli_list_history_candidates` | 无 | `HistoryCandidateView[]` |
| `multicli_update_history_source` | `query:HistorySourceUpdate` | `HistorySourceView` |
| `multicli_refresh_history` | `query:{ sourceId? }` | `HistoryOverallStatus` |
| `multicli_get_history_status` | 无 | `HistoryOverallStatus` |
| `multicli_rebuild_history_index` | `query:{ sourceId }` | `HistoryOverallStatus` |
| `multicli_get_session_metadata` | `{ sessionId }` | `SessionMetadataView` |
| `multicli_list_session_messages` | `{ query:MessagePageQuery }` | `MessagePage` |
| `multicli_search_history` | `{ query:HistorySearchQuery }` | `SearchPage` |
| `multicli_resolve_resume_target` | `{ sessionId }` | `ResumeTargetView` |

### 1.1 为什么多出 `multicli_list_history_candidates`

卡片允许“可列已知候选供选择”，并要求“来源启用必须由用户操作、不能首次启动自动遍历
所有历史”。候选命令只对用户主目录下三个声明的候选根做**深度 0**的目录判定与最多 200
个顶层条目的有界计数，不递归、不读文件、不自动登记来源。没有它，用户必须在 UI 里手打
绝对路径，反而更容易填错。

### 1.2 命令注册

只在实现完成并通过本契约验证后注册。`multicli_baseline_info`、`multicli_shell_smoke`
仍是开发诊断命令，不属于本契约。没有提前注册 `start / send / kill / history-edit`。

## 2. 来源

### 2.1 `HistorySourceUpdate`

```json
{
  "sourceId": "hsrc-… | null",
  "product": "codex | claude-code | codebuddy-code",
  "installationId": "install-… | null",
  "configProfileId": "unconfirmed-local",
  "root": "D:\\data\\codex-sessions",
  "enabled": true
}
```

- `product` 同时接受 `codex` / `claude` / `claude-code` / `codebuddy` /
  `codebuddy-code`，落库为规范键。
- `configProfileId` 1..64 字符；本契约 UI 默认 `unconfirmed-local`，不代表真实账号。
- `root` 必须是绝对路径、存在、可读、是目录。保存时会真的尝试列出一个条目，
  因此“目录存在但拒绝列出”在配置阶段就返回 `access_denied`，不会拖到索引时才失败。
  以下边界会被拒绝
  （`invalid_argument`，`details.reason` 给出固定原因）：
  - 盘符根（`C:\`）与 UNC 根；
  - 用户 profile 根（`%USERPROFILE%`）；
  - `%APPDATA%` 本身；
  - 含凭据/系统目录名的任意层级：`.ssh`、`.gnupg`、`.aws`、`.azure`、`.kube`、
    `.docker`、`credentials`、`credential manager`、`microsoft\credentials`、
    `user data`、`windows`、`program files`、`program files (x86)`、
    `programdata`、`$recycle.bin`、`system volume information`。
  - 不存在 → `not_found`；拒绝访问 → `access_denied`；其他 I/O → `io`（可重试）。
- 扫描时还会跳过根下任何符号链接/重解析点（junction），不跟随。
- 同一 `(hostId, agentId, configProfileId, canonicalRoot)` 是同一来源，重复提交返回原
  来源（幂等）。**已存在 sourceId 的 canonical root 不可原地改写**（返回 `conflict`）；
  换目录请新建来源。这样不会在用户不知情时把旧来源的派生数据挂到新目录上。

### 2.2 `HistorySourceView`

| 字段 | 说明 |
|---|---|
| `sourceId` | 确定性 id：`hsrc-` + host/agent/profile/canonicalRoot 的 SHA-256 前 40 hex |
| `hostId` / `agentId` / `product` | 归属 |
| `installationId` | 可空；不伪造安装 |
| `configProfileId` / `configProfileStatus` | 后者本契约恒为 `unconfirmed` |
| `root` | `HostPath`（display + canonical） |
| `enabled` | 只影响索引任务，不删除源文件，也不删除已导入的会话 |
| `parserVersion` | 映射代次；升级会使命中游标失效并触发重读 |
| `createdAt` / `updatedAt` | UTC 毫秒 |
| `status` | 见 2.3 |

### 2.3 `HistoryStatusView`

`complete` 的含义是“**列出的每个文件都已进入索引**”：任一文件停在预算上
（`filesIncomplete > 0`）、有文件被拒绝（`filesFailed > 0`）、或有记录被隔离/拒绝
（`lineErrors > 0`）时为 false。状态与它一致：停止在预算上而没有错误时是 `partial`，
不会伪装成 `ok`；`lastSuccessAt` 只在整个 pass 真正完成时更新。
`filesIncomplete` / `lineErrors` 通过 `error.details` 暴露（无错误时在 `note` 里说明）。

| 字段 | 说明 |
|---|---|
| `generation` | 该来源的索引代次，每次尝试 +1 |
| `status` | `idle / indexing / ok / partial / error / cancelled` |
| `inFlight` | 是否有该来源的 pass 在途 |
| `lastAttemptAt` / `lastSuccessAt` | 分开记录；失败只推进 attempt |
| `error` | 真实 `AppError`；无失败为 null |
| `filesTotal/Done/Skipped/Failed` | 本次 pass 的计数 |
| `complete` | 见上：全部列出文件都已进入索引才为 true |
| `note` | 人类可读的本次结论；不代替 `error` |
| `messagesIndexed` | 本次 pass 新插入的消息数（重复导入为 0） |
| `bytesRead` | 本次 pass 消费的字节数 |
| `complete` | 是否在预算内走完整个配置根 |
| `progressPermille` | 0..1000，UI 进度条 |
| `note` | 固定说明或本次 pass 的摘要 |

失败语义：

- 单文件失败（格式不符、不可读）→ `partial`，`error` 非空，已完成的文件已提交；下次
  pass 重试其余文件。
- 整次 pass 抛错 → `error`，旧数据不变、`lastSuccessAt` 不前进。
- 取消 → `cancelled`，已提交部分保留。
- **绝不把失败显示成“成功 0 条”**。

### 2.4 `HistoryOverallStatus`

```json
{ "schemaVersion": 1, "scope": "local-windows", "inFlight": true,
  "sources": [ { "sourceId", "agentId", "product", "enabled", "status": HistoryStatusView } ],
  "note": "…" }
```

`multicli_get_history_status` 供 UI 每 1.5 s 轮询一次；它不等待 pass 完成。
`multicli_refresh_history` 只把来源入队并立即返回当前状态：`generation` 在该 pass
真正开始时 +1，所以紧随刷新的一次响应可能仍是上一代次，随后由轮询推进。
应用重启时，上个进程遗留的 `inFlight=1` 会被清掉并记为 `cancelled`（不是 `ok`）。

## 3. 会话与消息

历史会话 `origin = history-only`，没有 Run（原记录没有协议证据，不伪造
native/acp/pty 连接）。`sourceNamespace = "history:<agent>:<product>:<configProfile>"`；
`(sourceNamespace, nativeId)` 唯一。会话 id 为确定性 `hist-` + SHA-256 前 40 hex。

### 3.1 `SessionMetadataView`

| 字段 | 说明 |
|---|---|
| `session` | 冻结的 `Session` DTO |
| `workspaces[]` | `workspaceId / role(primary\|related) / evidence / path?` |
| `runs[]` + `runTotal` | 最多 50 条；历史会话通常为 0。没有“按需更多”入口，因为本契约不产生 Run |
| `messageCount` | 该会话消息总数 |
| `messageKinds[]` | 最多 32 种 kind 及计数 |
| `firstMessageAt` / `lastMessageAt` | 时间边界 |
| `history` | 来源证据：sourceIds、文件数、parser 版本、`readOnly: true` |
| `note` | 固定说明：元数据不含消息正文 |

**禁止**在元数据里附带全量 messages；消息只能经 `multicli_list_session_messages` 有界读取。

### 3.2 `MessagePageQuery` / `MessagePage`

```json
{ "sessionId": "hist-…", "limit": 50, "cursor": null, "anchorMessageId": null }
```

```json
{ "items": [Message], "nextCursor": "…|null", "total": 123,
  "anchorMessageId": "…|null", "anchorIndex": 7 }
```

- `limit` 1..200；越界 `invalid_argument`。
- 顺序：`(observedAt, seq, id)` 升序。`total` 是同一会话的消息总数。
- 游标是 base64url JSON `{sort:"history-messages", sessionId, observedAt, seq, id, epoch}`。
  它绑定的是**某个会话里**的一个位置：属于别的列表、别的会话、非法 base64/JSON，或携带的
  `epoch` 与当前派生数据代次不同（期间发生过重建/截断导致行被删除）→ `invalid_argument`，
  绝不返回“成功但少了几条”的一页。
- `cursor` 与 `anchorMessageId` **互斥**（同时给出 → `invalid_argument`）。
- 锚点语义：先定位 `anchorMessageId` 在时间线中的位置 `p`，返回从
  `max(0, p - floor(limit/4))` 开始的最多 `limit` 条；`anchorIndex` 是锚点在该页中的下标。
  `nextCursor` 从本页最后一条之后继续。锚点不存在 → `not_found`（`entity:"message"`）。
- `anchorIndex` 只在带 `anchorMessageId` 时有值，其余情况为 `null`。
- 超大消息正文在导入时已截断（`content.truncated = true`）；工具调用的输入单独用
  `content.tool.inputTruncated = true` 标记，正文上限各自独立。分页不再二次截断。

## 4. 搜索

### 4.1 `HistorySearchQuery`

```json
{ "text": "历史记录", "workspaceId": null, "agentId": null, "sourceId": null,
  "limit": 50, "cursor": null }
```

- `text` trim 后 1..200 字符；空白 → `invalid_argument`。
- 过滤条件两两独立，`total` 是同一组过滤条件下的计数。
- `workspaceId` 命中会话的主要或关联工作区。

### 4.2 匹配策略

| 输入 | 策略 |
|---|---|
| ≥3 字符（含中文） | FTS5 `trigram` 表 `history_message_fts`，查询被包成双引号短语，内部 `"` 双写 |
| 1–2 字符 / 单个汉字 / emoji | 对 `history_message_fts.body` 做转义 `LIKE`（`\` 为转义符，转义 `% _ \`），查询与正文均小写化 |

- 不依赖英文命中来宣布中文可用：两种路径分别有单元测试与真实副本证据。
- FTS 操作符（`* - : ^ ( ) NEAR "`）在引号内一律按字面处理。
- 不使用 FTS 的 `snippet()`：片段在 Rust 中按字符窗口生成，纯文本、去控制字符与 bidi
  覆盖符，UI 只按文本渲染（不使用 `dangerouslySetInnerHTML`）。

### 4.3 `SearchPage`

```json
{ "items": [SearchHitView], "nextCursor": "…|null", "total": 42,
  "matchMode": "fts|like", "note": "…" }
```

`SearchHitView`：`sessionId`、`messageId`、`sessionTitle`、`agentId`、`kind`、`seq`、
`observedAt`、`snippet`、`snippetTruncated`、`matchMode`、`sourceIds`、
`sessionMessageCount`（该会话的**全部**消息数，不是本页条数）。

- 排序：`session.createdAt DESC, message.observedAt ASC, seq ASC, id ASC`，稳定且可跨页。
- 搜索游标：base64url JSON `{sort:"history-search", sessionCreatedAt, observedAt, seq, id,
  query, filters, mode, epoch}`。`query` 是规范化后关键字的指纹，`filters` 是
  `workspaceId/agentId/sourceId` 的指纹，`mode` 是 `fts|like`，`epoch` 是派生数据代次。
- 游标是**键集分页**，绑定的是排序键而不是某个索引代次：后台导入在游标之后新增的命中会
  出现在后续页；插到游标之前的行不会被追加到已翻过的页里，重新查询即可看到。这样翻页
  不会因每次导入而整体失效，也不会重复返回同一行。
- 游标**携带查询指纹**：把 A 关键字的游标用于 B 关键字、换一组过滤条件、匹配模式变化
  （3 字↔2 字），或期间发生过重建/截断，都会明确返回 `invalid_argument`（`field: cursor`），
  不会静默跳过记录。UI 在关键字或过滤变化时丢弃游标并重查第一页；直接调用 IPC 时同样
  按此约定处理即可。
- 结果不复制会话：`sessionId` 与浏览入口是同一主记录；点开后用
  `multicli_get_session_metadata` + `multicli_list_session_messages(anchorMessageId)` 定位
  同一条消息。

## 5. 继续目标（只解析）

`ResumeTargetView`：

| 字段 | 说明 |
|---|---|
| `sessionId` / `agentId` / `product?` / `nativeId?` / `sourceNamespace` | 同一会话身份 |
| `configProfileId?` | 来自来源配置 |
| `status` | `resolved / blocked / unknown / unsupported` |
| `reasonCode` | 稳定的本地化原因码：`resolved`、`missing-native-id`、`agent-not-in-history-products`、`active-run`、`live-process-same-workspace`、`live-process-unlinked`、`installation-not-confirmed`；前端对未知码显示通用受限说明，不推断成功 |
| `reason` | 原始诊断说明，收在技术详情中 |
| `installationId?` / `executable?` | 已登记安装；不猜 |
| `workspaces[]` | 会话工作区 |
| `conflicts[]` | `{kind, detail}`：`active-run` / `live-external-process` / `missing-native-id` |
| `evidence[]` | 固定证据标签 |
| `executionAvailable` | 仅`resolved`目标为`true`；只表明可通过独立`runtime_resume`显式继续，此解析调用仍不启动进程或发送消息 |
| `note` | 固定说明：不启动进程、不发 prompt、不写 CLI 文件 |

判定顺序（`blocked` 是最强的确定性结论，优先于“不支持/不确定”）：

1. 该会话有 `starting/running/stopping` 的 Run → `blocked`（`active-run`）；即使同时
   缺少 `nativeId`，也报告 `blocked`，因为写入租约冲突是已确认事实。
2. 无 `nativeId` → `unsupported` + `missing-native-id`。
3. 无对应产品 → `unsupported`（`agent-not-in-history-products`）。
4. 同产品存在 `alive` 的外部实例：
   - 若其显式目录参数对应的已登记工作区与目标会话主工作区相同 → `unknown` +
     `live-external-process`（无法证明是同一原生会话）；
   - 否则 → `unknown`（无法把任意同产品进程当作空闲证据）。
5. 没有已确认安装 → `unknown`。
6. 其余 → `resolved`。

**不以“未匹配进程”认定空闲**；打开历史不等于恢复成功；真正执行由结构化运行时负责。

## 6. 历史索引表的迁移演进（当前整体v7）

v4 新增（不改既有表、ID、外键）：

- `history_sources`：来源配置（用户选择，不是 CLI 配置副本）。
- `history_index_state`：每来源状态与计数。
- `history_cursors`：`(sourceId, fileIdentity)` → 文件身份、committedOffset、tailResidue、
  parserVersion、错误计数。
- `history_message_origins`：消息 → 来源/文件/字节偏移/行号，用于“只清本来源派生数据”。
- `history_session_facts`：会话 → 来源/nativeId/title 来源。
- `history_message_fts`：FTS5 `trigram` 索引（普通表，非 external-content）。

v5 在此基础上修正三处完整性缺口：

| 变更 | 原因 |
|---|---|
| `history_message_origins` 主键改为 `(message_id, source_id, file_identity)` | 同一来源的两个文件可能提供同一条消息；旧主键让后处理的文件覆盖前一个，截断副本会删掉另一个文件仍提供的消息 |
| `history_cursors` 增加 `lines_committed`、`read_complete`、`parser_state_json`、`verify_offset`、`verified_at_ms` | 只记录字节偏移无法区分“读到 EOF”和“预算用完”；行号每 pass 从 0 开始会让 fallback event id 冲突；追加时没有 header 就丢失 native id |
| 新增 `history_file_blocks`、`history_meta` | 分块内容哈希用于有界复核；`history_meta.purgeEpoch` 是派生数据代次，用于让过期游标明确失败 |

v6 再补一处：分块表增加 `layer`（`logical` / `physical`），主键改为
`(source_id, file_identity, layer, block_index)`。`logical` 覆盖解码后的记录，普通 `.jsonl`
用它；`physical` 覆盖磁盘上的原始字节，压缩流用它——否则压缩文件只能“整段解码后比对”，
超过一个固定阈值就永远无法复核。v5 的既有行原样复制为 `logical`，迁移不删任何现场数据。

迁移在既有版本化备份 + 单事务机制内执行；v1/v2/v3/v4/v5 → 最新版本连续升级，失败整体回滚。
v5 重建 `history_message_origins`、v6 重建 `history_file_blocks`（均关闭外键 + `foreign_key_check`），
其余为加列/建表。
**v4 游标没有 parser 检查点，迁移后第一次扫描会对该文件做一次完整重读**（而不是猜行号），
因此升级后首个 pass 会把历史索引补齐并写入块哈希。

### 6.1 派生数据与用户数据

- 可重建：`messages`（仅无任何来源贡献的）、`history_message_origins`、`history_cursors`、
  `history_file_blocks`、`history_message_fts`。
- 永不重建：`sessions`（稳定 id）、`user_session_notes`、`user_session_tags`、其他来源的
  消息、安装与进程观察。
- 一条消息只要还有**任意一个** `(来源, 文件)` 贡献就保留；截断/替换/重建某个文件只删除
  它自己贡献过、且再无其他贡献者的消息。稳定会话 id 与其备注/标签不受影响。

### 6.2 续读与内容复核

- 游标保存 `readComplete`。未到 EOF（预算/时间/取消/解压上限）时该值为 0，pass 的
  `complete` 为 false，下一 pass 从 `committedOffset` 继续；尺寸与 mtime 未变也不会被当成
  “已处理”。
- 压缩流（`.jsonl.zst`）的 `committedOffset` 是**解压后**的逻辑偏移，物理大小另存；续读时
  从头解码并丢弃已提交前缀，丢弃过程受时间预算与解压上限约束，可被取消。
- 每个文件按 256 KiB 分块保存哈希。复核是**独立阶段**：一次 pass 先导入（新文件/追加/
  替换），再用预留的一段预算复核——从 `verifyOffset` 前进、走完整文件后回到 0。命中不一致
  → 在同一事务语义下整文件重建（只删该文件贡献且再无其他贡献者的消息）。
  **追加不再免检**：文件每轮都增长时，`verifyOffset` 仍按窗口推进，固定已提交前缀的检测
  上界与“文件是否还在写”无关。追加只会重写游标所在的那个块，且写前先比对它的旧哈希，
  因此新块不会把旧改写“吸收”掉。
- **压缩流按物理字节复核**：`.zst` 的 `committedOffset` 是解压后的逻辑偏移，用逻辑块做窗口
  需要每次都从 0 解码（二次复杂度）。因此压缩文件另存一份 `physical` 分块（磁盘原始字节，
  256 KiB/块），复核对它做同样的有界窗口；任何变化（包括同大小、同 mtime 的重新编码、
  skippable 帧变化）都会被发现，随后整段重解码重建。物理基线在导入事务内、解码之前顺序
  读取一次生成，因此“解析时基线”和“复核基线”是同一批字节。
- **检测上界**：一次完整复核周期 = `ceil(复核字节数 / 1 MiB)` 个窗口；pass 的 8 MiB 预算按
  轮转顺序分给文件（每文件可用到自己的公平份额；预算用尽时后面的文件本 pass 拿不到窗口，
  但轮转保证它下一次排在前面），轮转位置持久化、重启不回到表头。因此固定已提交前缀最坏在
  `ceil(字节数 / 1 MiB) × ceil(文件数 / 每 pass 覆盖数)` 个 pass 内被完整比较一次，再加一次
  冷却。字节位置不随追加移动，所以持续追加不会延长这个上界。
- **冷却**：一个文件完成整周期后 15 min 内不再重复复核；同大小但 mtime 变化的改写会绕开
  冷却（这是原地改写的特征）；长度变化的文件（追加/截断）不绕开——新字节是导入时刚写下的，
  会由下一次周期覆盖。
- **预算下限**：`verifyBudgetBytes` 为 0 表示本 pass 不复核；任何正数都会被抬到一个块
  （256 KiB）——块是带哈希的最小单位，低于它的预算无法比较任何东西，会让文件永远停在
  “待复核”。
- **公平性**：复核顺序按持久化的轮转位置推进，预算用尽即停并记录位置；一个正在复核的大
  文件不会永远占住预算，列表尾部的文件也不会饿死。
- **压缩上限是导入前的诚实拒绝**：物理大小 > 128 MiB 的 `.zst` 在解码前返回
  `unsupported`（`reason=compressed-too-large`），旧行原样保留；不会出现“导入成功但永远
  等不到复核”。解压上限 256 MiB 仍然独立生效。
- 超大行（> 1 MiB）被隔离为标记行并记录 `line-too-large`；单行超过 1 MiB + 16 MiB 的跳过
  上限时整文件报错，不会无限读取。

## 7. 有界性

| 限制 | 值 |
|---|---|
| 目录深度 | 12 |
| 单次列出的文件数 | 50,000 |
| 单文件单 pass 读取 | 32 MiB（解析）；压缩文件的物理基线与复核窗口另计，各受同一事务预算约束 |
| 单文件单事务时间预算 | 1.5 s（增量）/ 30 s（重建） |
| 单 pass 时间预算 | 4 s（增量）/ 30 s（重建） |
| 内容复核 | 8 MiB/pass 按轮转分给文件，单文件可用到自己的公平份额（窗口上限 1 MiB、下限 1 块 256 KiB）；正预算不足一块时抬到一块，0 表示关闭 |
| 压缩历史物理大小 | 128 MiB（超过则在导入前拒绝，保留旧数据） |
| 压缩历史解压后大小 | 256 MiB/pass（解压炸弹上限） |
| 复核轮转 | 位置存 `history_meta`（`verifyPath:<sourceId>`），重启后继续 |
| 压缩解压上限 | 256 MiB/文件/pass |
| 单行上限 | 1 MiB（超出→显式隔离标记，不丢整文件） |
| 单行跳过上限 | 16 MiB（超出→`unsupported`，不扫描巨型行） |
| 格式探测 | 64 行；期间无任何可识别记录才判定选错产品 |
| 单消息正文 | 256 KiB |
| 搜索正文 | 64 KiB/消息 |
| 自动补扫间隔 | 30 s（监听只作加速；本契约没有文件监听，靠周期补扫） |
| 轮转分片 | `*.jsonl.<1-3 位数字>` 与主文件一起列出；分片独立游标、独立身份 |

列目录本身有界（深度 12、50,000 文件、访问预算），但不计入单 pass 的解析预算；
否则 10k 文件的根会把绝大部分预算花在 `read_dir` 上，永远走不到列表末尾。
列表按片段消费：一次 pass 从上次报告 `nextIndex` 的位置开始、循环到列表末尾；预算用尽
时返回新的 `nextIndex`，下一次从那里继续，不会每轮都重查同一段前缀。

## 8. 错误

沿用冻结 `error.schema.json`。常见映射：

| 场景 | code |
|---|---|
| 参数越界/空白/互斥游标 | `invalid_argument` |
| 来源/会话/消息不存在 | `not_found` |
| 已知 sourceId 换根 | `conflict` |
| 源目录不可读 | `access_denied` / `io`（retryable） |
| 历史文件格式不符 | `unsupported`（`details.reason = "unrecognized-format"`，`retryable=false`） |
| 游标属于别的查询/会话/代次 | `invalid_argument`（`field:"cursor"`） |
| 单行超过跳过上限 | `unsupported`（`details.reason = "line-too-large"`） |
| 解压超过上限 | `unsupported`（`details.reason = "decompressed-too-large"`） |
| 数据库失败 | `database` |

## 9. 安全

- 历史内容不可信：不执行 HTML/脚本/命令/本地链接/控制序列；UI 只做纯文本渲染。
- 未知字段只保留**字段名**（过滤字符集与长度），从不保存值；token/cookie/认证字段不会
  进入数据库、日志或 UI。
- `sourceRef` 形如 `path:<hash>#L<行号>`，不含真实路径。
- 诊断文件不新增正文、nativeId 或用户路径。

界面“继续会话”由一次明确点击触发：先调用只读解析，仅当返回同一 `sessionId`、`status=resolved` 且 `executionAvailable=true` 时，转交独立 `runtime_resume`。查看会话不自动检查或执行。检查期间禁用按钮，切换会话或卸载后丢弃晚到结果。旧二进制缺少 `reasonCode` 时，只从已有稳定冲突/证据码推导提示，原文保留于技术详情。
