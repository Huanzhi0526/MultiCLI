# MultiCLI 架构

适用版本0.1.0。Windows本地Tauri 2应用：React/TypeScript界面，Rust领域服务，SQLite WAL持久化，ConPTY/xterm托管终端。

## 当前启动与数据流

```text
src/main.tsx -> src/multicli/App.tsx -> api.ts/types.ts
                         | Tauri IPC: multicli_*
src-tauri/src/lib.rs -> multicli_bootstrap.rs
                         | Runtime / service / lifecycle
src-tauri/src/multicli/{discovery,history,terminal,runtime,usage,backup}
                         | SQLite / owned processes / read-only sources
%LOCALAPPDATA%/MultiCLI/<profile>
```

lib.rs只接当前multicli_bootstrap。保留的上游startup、旧数据库、插件、relay和自动更新源码不属于当前产品启动图。
增加功能优先在当前模块中扩展，不通过恢复上游启动入口顺带开放其他后台行为。

## 领域身份

| 实体 | 语义 |
|---|---|
| Host / HostPath | 本机主机与带主机身份的路径；本版只有local-windows |
| Workspace | hostId与canonicalPath确定身份；两个worktree是两个工作区 |
| AgentDefinition / Installation | 产品族与具体安装/配置入口，发现不等于可用或已登录 |
| Session | 跨运行的逻辑会话，原生身份含产品/配置命名空间 |
| Run | 一次执行或继续，独立生命周期；继续复用Session而新建Run |
| Process | hostId、PID、creationTime共同确定实例，避免PID复用混淆 |

工作区和Agent两种导航查询同一Session/消息数据。来源与通信方式分开：managed、external-observed、history-only描述来源；native、ACP、PTY描述传输。
外部进程只读观察；历史存在不表示进程活跃或具备输入输出接管权。

## 模块与异步行为

- discovery：签名、入口/包装器识别、Win32进程快照、扫描代次和脱敏视图。数量预算与不可读目录须报告扫描不完整。
- history：只读来源扫描、格式解析、增量游标、轮转/截断/坏行恢复、搜索和继续目标解析。
- terminal：安全argv启动、ConPTY、Job Object、自有进程身份、有限环形缓冲与日志、订阅/resize/停止。
- runtime：三核心协议适配、Run状态、正文/工具/权限事件、取消和继续。请求和晚到事件必须保持Run归属。
- usage：官方额度、来源明确的本地统计、账户池、退避和新鲜度；未知数值不能变成0。
- backup：当前profile快照、清单校验、下次启动恢复、提交点与失败回滚。

前端请求按作用域隔离，旧请求晚到不能覆盖新的选择。文本输出的存储、传输和显示应分别处理，避免动画或批处理丢字。
更细的字段和状态语义见[contracts](contracts/README.md)，文件定位见[SOURCE-MAP](research/SOURCE-MAP.md)。

## 持久化与后台边界

SQLite当前schema v7，版本迁移在应用自有库完成，不写入各CLI原生库。
release默认prod，debug默认dev，test独立；可用进程级MULTICLI_DATA_ROOT绝对路径覆盖。WebView缓存、诊断、备份与终端日志均属于当前profile。
关闭窗口到托盘继续工作；明确退出清理自有进程和后台任务。应用崩溃不保证终端保活。
当前没有RemoteHost实现、SSH服务、云同步、遥测或应用自动更新发布点。
