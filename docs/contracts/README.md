# MultiCLI 产品契约 v1

适用产品0.1.0，整体SQLite schema v7。领域DTO与IPC使用camelCase；契约版本与数据库版本分开。变更需同步生产方、消费方和兼容行为，不能静默重解释字段。baseline诊断命令不属于常用业务接口。

## 身份与持久化约束

- Host 仅 `local-windows`；HostPath 必带 hostId、displayPath、canonicalPath。本机正规化处理盘符、大小写及可访问目录别名；不能把所有未来主机的路径统一小写。不可访问目录标错误或未解析，不能猜 canonicalPath。
- Workspace 的唯一键为 `(hostId, canonicalPath)`；repoGroupId 是仓库聚合，两个 worktree 保留独立 workspaceId。
- Session、Run、Process 分表。Session 原生唯一键 `(sourceNamespace,nativeId)`，nativeId 未取得时可空；来源命名空间必须含产品/安装 profile 范围。不得让 PID 成为 Session 主键。
- 同一 Session 的 Workspace 与 Agent 两条导航查询引用同一主记录、消息和计数；关联表 SessionWorkspace 保存 role 与 evidence。resume 新建 Run，沿用 Session。
- Process 身份为 hostId + pid + creationTime。PPID 需验证父创建时间；PID 复用不得覆盖旧进程的会话归属。外部观测不拥有终端或停止权。
- SQLite WAL；版本化迁移、单写路径、外键。索引派生数据与用户备注/标签分离，清理/重建索引不清自有数据。外部源始终只读，不复用上游 history/edit 入口。

## 运行及事件

Run.executionStatus：starting → running → stopping → exited/failed；starting 可直接 failed，任何中间状态允许 unknown（附原因），unknown 不能制造退出结论。终态不可被晚到事件重开；重试新建 Run。

交互状态独立为 working / awaiting-permission / awaiting-input / idle / unknown，附 observed/inferred 和 evidence。无输出/低 CPU 不单独触发 awaiting-input。

事件 envelope 见 events.schema.json。seq 只在 `(runId,streamId)` 内单调，不能以跨源到达顺序冒充原始顺序。按 nativeEventId 或稳定 sourceKey 去重；重复与未知 kind 允许保留，未知 schemaVersion 不自动执行。流关闭/取消后仍需收敛当前 Run，不能转给新 Run。

PermissionRequest 带 requestId、runId、原生请求 id、实际选项及 pending/answered/expired/cancelled 状态。默认不回答，不自动允许；回复只能命中仍有效的同一 Run 请求。先取消再收到批准不能执行。官方协议不具备能力时返回 unsupported 或 unverified，不伪造确认卡。

## IPC 与接口边界

所有产品命令前缀 `multicli_`；Tauri 仅注册当前已实现并接入当前产品的命令。前端不拼接系统命令，也不读取凭据。成功返回数据；失败 reject 一个 `AppError`（error.schema.json），不得把失败包装为成功空数组。列表命令使用 `limit`（1..200）、可空 cursor，响应 `{items,nextCursor,total}`；total 为同一过滤条件计数。默认顺序须稳定并加 id 次排序。

当前导航命令包括`multicli_list_workspaces`、`multicli_list_agents`、`multicli_list_sessions`、`multicli_get_session`、`multicli_list_capabilities`。列表参数为`{query:{workspaceId?,agentId?,origin?,limit,cursor?}}`，详情输入`{sessionId}`。空库显示空状态，合成fixture仅用于测试，不在正式启动插入演示数据。

发现与路径识别由discovery及path模块负责；协议由RuntimeManager和各产品adapter负责；历史由HistoryEngine及parser负责。启动使用受限argv、工作目录与进程级环境差量，不拼接任意shell字符串。

TerminalManager提供open/read/write/resize/stop/close。输出通知与按游标read分开，停止与关闭分开；外部进程不提供通用接管接口。cancelTurn不等于kill，结构化协议与来源所有权分开。

额度由UsageCenter与provider模块提供；账户官方身份不可得时保留未确认配置，不把不同来源账户池混合。当前没有RemoteHost实现、SSH服务或远程设置页。

## 能力及用量

能力 supported/unsupported/unverified 与证据层 document/fixture/live 分开，带产品版本、来源、验证时间、失败原因。版本/帮助输出 不是 live structuredChat PASS。

QuotaPool、Snapshot 按账户池/窗口唯一，不按会话相加；UsageSample 可关联 Run，但不能将协议和历史中同一 token 再次计数。订阅额度、API 账单、CLI token、本地估算各自保留来源、单位、时间及 stale/error。缺失数值为 null，不能填 0 或据 token 推余额；API key/cookie 不存数据库。
