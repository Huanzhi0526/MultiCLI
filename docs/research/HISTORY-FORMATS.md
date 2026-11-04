# 历史格式技术样本（2026-11-02）

核对日期：2026-11-02（Asia/Shanghai）。核对对象是本机实际安装并实际产生过会话的三款
CLI；当时版本来自已观察的 `--version` 探测：

| 产品 | 探测版本 | 历史根（相对 `%USERPROFILE%`） |
|---|---|---|
| Codex CLI | 0.166.0 | `.codex\sessions` |
| Claude Code | 2.1.153 | `.claude\projects` |
| CodeBuddy Code | 2.179.0 | `.codebuddy\projects` |

本文件是保留的格式样本，不表示这些CLI版本仍为当前安装；当前产品覆盖以COMPATIBILITY.md为准。

本文件只记录**实际观察到的结构**。没有把上游 README、旧版解析器或另一产品的字段当成
兼容性保证。真实正文、账号、路径与凭据不进入本文件、fixture 或 Git。

## 1. 目录与文件形态

### Codex

- `sessions\<YYYY>\<MM>\<DD>\rollout-<timestamp>-<uuid>.jsonl`
- 同目录也存在 `rollout-…jsonl.zst`（zstd 整文件压缩）。本次核对时 172 个未压缩、294 个
  压缩文件。
- 未压缩文件是追加写：会话进行中不断变长，末行可能不完整。
- 压缩文件是整文件重写：文件身份变化即视为新内容，整段重读。

### Claude Code

- `projects\<project-slug>\<session-uuid>.jsonl`
- `projects\<project-slug>\memory\*.md` 是记忆文档，不是会话记录，不导入。
- 也观察到 `projects\<project-slug>\<session-uuid>\subagents\agent-*.jsonl`：子代理记录。
  按普通JSONL导入，`isSidechain` 写入消息内容，不另造会话。

### CodeBuddy Code

- `projects\<project-slug>\<session-uuid>.jsonl`
- `sessions\*.json` **不是会话记录**：内容是 `pid`、`processIdentity`、`lastHeartbeat`、
  `endpoint`、`kind` 等运行态字段。它们没有消息，也没有稳定会话正文，因此不作为历史源；
  导入时会因扩展名不符被跳过（只接受 `.jsonl`）。

## 2. 记录包装

### Codex

每行 `{ ordinal, timestamp, type, payload }`，`type` 取值：

| type | 处理 |
|---|---|
| `session_meta` | 原生会话：`payload.session_id`（或 `id`）、`cwd`、`cli_version`、`runtime_workspace_roots` |
| `response_item` | `payload.type` ∈ `message` / `function_call` / `custom_tool_call` / `function_call_output` / `custom_tool_call_output` / `reasoning` / `compaction` |
| `event_msg` | `user_message` / `agent_message` / `turn_aborted` / `error` 等 |
| `compacted` / `compaction` | 摘要记录 |
| `turn_context` / `world_state` / `token_usage_record` | 有意忽略（上下文、世界状态、计费，不是聊天消息） |

### Claude Code

每行带 `sessionId`、`uuid`、`parentUuid`、`cwd`、`timestamp`、`version`、`isSidechain`
与（通常）`message:{role, content}`：

| type | 处理 |
|---|---|
| `user` / `assistant` | 按内容块拆分：`text`、`thinking`、`tool_use`、`tool_result`、`image` |
| `system` | `subtype` + 文本 → `system` 消息 |
| `attachment` | 只保留附件类型名（`[image]` 等），不导入附件正文 |
| `summary` | 摘要消息 |
| `ai-title` | 原生标题，更新会话标题，不产生消息 |
| `mode` / `permission-mode` / `file-history-snapshot` / `last-prompt` / `queue-operation` | 编辑器/队列状态，忽略 |

### CodeBuddy Code

| type | 处理 |
|---|---|
| `session-meta` | `sessionId` / `id`、毫秒时间戳 |
| `message` | `role` + 块数组 → 用户/助手消息 |
| `reasoning` | `content` / `rawContent` 文本 → 推理消息 |
| `function_call` | `name`、`arguments`、`callId` → 工具调用 |
| `function_call_result` | `output:{type,text}`、`status` → 工具结果 |
| `ai-title` | 原生标题（不产生消息） |
| `summary` | 摘要消息 |
| `file-history-snapshot` | 编辑器快照，忽略 |

## 3. 标准化映射

| 目标 | Codex | Claude | CodeBuddy |
|---|---|---|---|
| 原生会话 id | `payload.session_id`/`id`；缺失时取文件名尾部 UUID | 每行 `sessionId`；缺失时文件 stem | `sessionId`；缺失时 `session-meta.id` |
| 原生事件 id | `payload.id` → `call_id` → `type:ordinal` | `uuid`（块再追加 `#<i>`） | `id` |
| 去重键 `sourceKey` | `codex:response:<id>` / `codex:event:<turn>:<type>:<ordinal>` | `claude:message:<uuid>#<i>` | `buddy:message:<id>` / `buddy:call:<id>` / `buddy:result:<id>` |
| `seq` | 顶层 `ordinal`，缺失时行号 | 行号 | 行号 |
| 时间 | 顶层 `timestamp`（ISO） | `timestamp`（ISO） | 毫秒整数 |
| 工作区 | `session_meta.cwd`（primary）+ `runtime_workspace_roots`（related） | 每行 `cwd` | 每行 `cwd` |
| 标题 | 首条真实用户消息（跳过 XML 标签形态的注入上下文） | `ai-title`，其次首条用户消息 | `ai-title`，其次首条用户消息 |

- 时间统一规范化为 UTC 毫秒 ISO；无原生时间时使用文件 mtime，并在
  `content.timeSource = "file-mtime"` 明示，不伪装成原生时间。
- 标题优先级：`native-title` > `first-user-message` > 空。原生标题到达前已写入的猜测标题
  会被原生标题替换；反之不会。
- 工作区按 canonical path 正规化，`primary` 证据 `history-cwd`，附加目录证据
  `history-workspace-root`。未解析的 cwd 不绑定工作区，不默认绑定 MultiCLI 当前项目。

## 4. 消息内容形状

所有消息 `content` 是对象，只含白名单字段：

```json
{
  "role": "user | assistant | system | tool | developer",
  "text": "…",
  "blocks": ["text", "image"],
  "tool": { "name": "Bash", "callId": "…", "input": "…", "status": "completed" },
  "truncated": false,
  "sidechain": false,
  "timeSource": "native | file-mtime",
  "unknownFields": ["fieldName"]
}
```

- 从不写入：`encrypted_content`、签名、token 用量、附件二进制、API key/cookie、原始
  命令行、完整未知字段值。
- 推理块只有摘要文本；Codex 加密推理只记录 `"encrypted": true`。
- `unknownFields` 只保留字段名（长度 ≤40，字符集 `A-Za-z0-9_-.:`，最多 32 个）。
- 正文、工具输入/参数、会话标题在入库前都会去除控制字符与 bidi 覆盖符（U+202A–202E、
  U+2066–2069），避免终端转义或“特洛伊木马”式重排影响读者；HTML 只按文本节点渲染。

## 5. 去重与异常

- 消息 id = `hmsg-` + SHA-256(`sessionId` + `sourceKey`) 前 40 hex；重复导入幂等。
- `messages` 仍有 `UNIQUE(session_id, source_key)` 与 `UNIQUE(session_id, native_event_id)`
  兜底。
- 同一原生会话出现在两个已配置根（文件被移动/复制）时：`(sourceNamespace, nativeId)` 相同
  → 同一 Session；消息按 `sourceKey` 去重。
- 未完成尾行不导入，游标停在最后一个换行之后；补齐后下一次 pass 只导入新增行。
- 坏行记录 `invalid-json` + 行号，继续处理后续行；不丢整个文件。
- 单行超过 1 MiB：无法作为 JSON 解析，导入一条 `recordType: "oversized-line"` 的隔离标记
  （含 `lineBytes`、`truncated: true`），并记录 `line-too-large` 错误。
- 文件被替换（前缀 hash 变化）→ 同一事务内清掉该文件旧派生行后重读；截断到 0 字节同理。
- 文件移动：路径身份变化 → 新路径整读一次（消息去重），旧游标在完整 pass 后删除。
- 文件轮转：`session.jsonl` 旁出现 `session.jsonl.<1-3 位数字>` 编号分片时，分片按同一产品
  格式读取。分片路径身份不同 → 独立游标，主文件重写不会清掉分片的行；重叠事件按
  `sourceKey` 去重。若分片从中途截断（没有会话头），会以 `unrecognized-format` 报告，
  不并入无关会话。

## 6. 已观察但未映射 / 已知缺口

| 产品 | 未映射内容 | 影响 |
|---|---|---|
| Codex | `world_state`、`turn_context`、`token_usage_record`、`compacted` 的替换历史 | 上下文/计费不展示；不影响消息时间线 |
| Codex | `response_item.reasoning.encrypted_content` | 只标记存在，不展示密文 |
| Codex | `event_msg.token_count` | 用量由usage模块处理，避免把token当订阅余额 |
| Claude | `file-history-snapshot`、`mode`、`permission-mode`、`last-prompt`、`queue-operation` | 编辑器状态不展示 |
| Claude | `attachment` 的文件内容 | 只保留 `[image]` 等类型名 |
| Claude | `system` 的 `compactMetadata` / `usage` | 只保留 subtype 与文本 |
| CodeBuddy | `file-history-snapshot`、`_meta`、`providerData` | 编辑器/供应商内部字段不展示 |
| CodeBuddy | `sessions\*.json` | 运行态，不是历史；不作为来源 |
| 三者 | 未知新记录类型 | 保留 `recordType` + 字段名，正文为空，标为 `unknown` |

没有发现“某格式完全不可读”的核心缺口；三款都能列出并导入真实会话。压缩 Codex rollout
采用“变化即整读”，大文件会分多个 pass 完成，`complete` 会明确反映是否走完。

## 7. 真实副本核对（2026-11-02）

副本只放在项目根 `scratch/S03/copies/`，不进 Git。真实正文、真实查询原文与原始日志只留在
该目录，不复制进本文件、回交文档或任何 fixture；下面只有形态统计与**合成**查询示例。

探针在导入前后对每个副本文件做 SHA-256（`sourceHashes[].unchanged = true` 表示逐文件一致），
并在关闭、重开数据库后比较整条消息时间线的摘要（`restart.timelineMatches = true`）。

| 产品 | 副本大小 | 首个 pass 消息 | 第二个 pass 新消息 | 说明 |
|---|---|---|---|---|
| Codex（未压缩 + 压缩各 1） | 31,923 + 24,055 B | 7 | 0 | 压缩文件只含一条 compaction 标记 |
| Claude Code | 58,141 B | 21 | 0 | 含 thinking / tool_use / tool_result / attachment |
| CodeBuddy Code | 363,708 B | 108 | 0 | 含 reasoning / function_call / function_call_result |

匹配路径用合成词验证（真实查询原文不公开）：

| 合成查询 | 模式 | 期望 |
|---|---|---|
| `历史记录` | fts | 3 字中文，trigram 子串命中 |
| `插件` | like | 2 字中文，转义 LIKE 回退 |
| `电` | like | 单字中文，回退路径不为空 |
| `github` | fts | 英文、大小写不敏感 |
| `100%` | fts | 字面匹配，不把 `%` 当通配符 |

## 8. 压缩历史的完整性基线

`.jsonl.zst` 的字节偏移不是“文件偏移”：解码后第 N 个字节在磁盘上的位置由压缩流决定。用解压
后的分块做窗口复核，意味着每个窗口都要从第 0 字节重新解码，成本随窗口数平方增长；这也是旧实现
只能“整段解码一次或干脆不复核”的原因。

因此压缩文件同时保存两份分块哈希：

- `logical`：解码后的记录分块（与普通 `.jsonl` 相同），用于说明“这份内容是什么”。
- `physical`：磁盘原始字节分块（256 KiB/块），复核窗口只读它。它不关心 zstd 帧怎么组织，
  因此单帧、多帧、含 skippable 帧（`0x184D2A5x`）的尾部填充都能覆盖；任何字节变化——包括
  大小与 mtime 都不变的重新编码——都会让某个块不匹配，随后整段重解码重建。

物理基线在导入事务内、解码**之前**顺序读一次生成，覆盖长度以实际读到的字节数为准（不是目录
列表里的旧长度），所以“解析时基线”和“复核基线”是同一批字节。物理大小超过 128 MiB 的文件在
解码前被拒绝（`unsupported` / `compressed-too-large`），旧行保留：这比“导入成功后永远等不到
复核”诚实。解压后 256 MiB 的上限独立生效，用于防解压炸弹。

真实副本里 Codex 的压缩文件只有 24,055 B（一条 compaction 标记），合成用例另建了多帧、
skippable 填充与同大小重写的样本；真实正文与查询原文仍只留在 `scratch/S03/`。
