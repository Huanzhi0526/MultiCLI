# S01 阶段验收记录

2026-11-02，Sol。**S01 整阶段 ACCEPTED**，覆盖领域持久化、真实 IPC、双向导航、Windows 路径/worktree 身份及脱敏诊断。提交、推送和新 CI 分别核验。

## 范围与来源

主要开发为用户手动转交的 DeepSeek，实际配置 WorkBuddy / deepseek flash 1017 / 1M / max；显示名称不推断成 API model ID。整个剩余阶段基线为 `0810ca4e3d6de985a0a29f562c878d53f68d46a0`。不用 Baton，未新增 Agent。用户已调整为整阶段交接，不再分别派 S01-02/S01-03；原 plan 和历史记录保留。

数据层此前已独立通过并提交：38 项领域、3 项启动/ConPTY、13 项主管反例及 4 项构建边界。原 R1 和 Sol 四项相邻小修现场仍在 scratch/S01-01/acceptance-r1 与 acceptance，历史结论保留。

本次实现六个 multicli_ 产品命令，统一读取独立 multicli.db；两入口引用同一 Session、Run 和 Message。正常启动只初始化 schema 与本地主机，不生成演示业务数据。能力状态和 document/fixture/live 证据分开显示；合成能力记录不代表真实 CLI 支持。

## 独立验证

| 项目 | 结果与口径 |
|---|---|
| locked Rust check | PASS；保留 499 条既有上游模块告警 |
| 正式领域测试 | 49/49 PASS，0 ignored |
| 正式启动/IPC/ConPTY | 11/11 PASS；真实 Windows ConPTY、Tauri 测试传输的畸形参数 AppError、WebView 后备目录 profile 隔离 |
| 夹具工具保护测试 | 4/4 PASS；保留用户引用、相邻路径和不完整目录，拒绝生产 profile |
| 前端构建/边界 | PASS；构建边界 4/4 |
| 前端 Vitest | 23/23 PASS；组件 fake API 只作为单元层证据 |
| 主管独立源码反例 | 14/14 PASS，0 ignored；直接编译实际模块，合成 SQLite/junction，无替代业务实现 |
| A01 真桌面 | 当前 debug 原生二进制 + 真实 Vite/WebView2，test profile；Alpha、Beta、Codex 三处均打开 fixture-session-cross，6 条消息/3 个 Run，消息内容一致，刷新保留详情 |
| 真实分页 | 写入 24 条专用合成会话；Codex 20/25 → 点击加载更多 → 25/25，24 个标题全部唯一，末页无下一页；刷新重置为第一页 20/25，详情未串线 |
| A02 文件系统 | 正式测试创建中文/空格、大小写别名、junction 和两个真实 Git worktree；同 Host 别名去重、不同 Host 分离；同 repoGroupId 且 workspace/worktree 独立，失联目录不阻断已有会话 |
| A02 拒绝访问 | 普通权限只读解析 C:/D: System Volume Information 等系统保护路径，实际 access-denied、canonicalPath=null；没有改 ACL |
| A20 导出失败恢复 | 实窗导出目录被专用文件占用，显示 io/可重试；恢复目录后点击重试，生成实际 JSON |
| A20 数据库失败恢复 | 数据库路径被专用目录占用，显示 database 错误；恢复原 DB，不重启、点击重试，工作区/Agent/会话均恢复 |
| 正常空库 | 先保留原 DB，再以新空库启动；实窗无工作区/会话，agents/sessions 为 0，没有固定演示数据 |
| 诊断隐私 | 实际 UI 导出 JSON 白名单计数正确（27 sessions/11 messages），无消息、原生 id、安装/工作区路径或原 Agent ID；私密标记反例通过 |
| 收尾 | 移除本次 24 条样本后，15 张表全部逻辑行与验收前备份一致，外键无异常；原 3 条 fixture 会话保留，应用/WebView 和自有 Vite 已退出 |

统一 verify.ps1 Test/All 已纳入前端 Vitest、领域、启动及夹具保护测试，构建边界断言同步补齐，避免 CI 漏跑本次回归。

## 主管按授权直接补齐

没有另发返工卡。先保留失败现场，再修复和复验：

- 诊断原样输出 opaque Agent ID，私密标记反例实际失败；改为文档内匿名编号，保留统计。
- fixture 清理曾经外键级联删除非 fixture 会话，真实运行 retainedUserSessions=0；现在事务前检查用户引用并拒绝破坏性清理，重跑保留 1 条，外键无异常。
- scratch 名称曾经 junction 越界落库；现在先解析实际目标，重跑拒绝且无越界文件。既有不完整目录也保留，不递归删除。
- Rust canonicalize 的 verbatim 路径不被 Git worktree add 接受；仅为 Git 参数适配普通 Windows/UNC 表示，保留真实目标校验，中文双 worktree 夹具真实重跑通过。
- 畸形产品 IPC 参数统一返回冻结 AppError，不再漏出 Tauri 字符串错误。
- WebView 写入失败的后备目录按 profile 隔离；非法 profile 不参与路径拼接。
- 会话筛选缓存键用 JSON 元组，避免 opaque ID 中星号/分隔符与空筛选碰撞。

主要业务仍由 DeepSeek 实现。Sol 修改限于验收发现的边界、测试工具、IPC/隔离及验证集成。

## 证据与限制

独立证据在 scratch/S01/acceptance：submitted-source.json、final-tested-source.json、final-tested.patch、rust-final.log、unified-test-final.log、frontend-final.log、platform-final.log、native-build-final.log、fixture-boundary-*.json、actual-access-paths.log、ui-*.json、profile-final-comparison.json 及受测 Git 对照。反例在 scratch/S01-01/acceptance/sol-s01-diagnostics-before/after.log。原始数据库、路径明细、日志与产物不进 Git。

受测产品文件 1489 个，提交时分别核对原始 SHA256 与 Git EOL/clean-filter 规范化 blob。桌面二进制 SHA256：`7BB0F547D8B49750A1F5DE3E0FD88CE02ACAD826D0B14250EC3FD0F03BA21C89`。本页随验收源码保存，具体 commit/远端 SHA 由 HEAD、origin/main 与根 STATUS 记录，避免回填自身 SHA 反复提交。

数据层 CI 已独立核实 success：[41318041014](https://github.com/Huanzhi0526/MultiCLI/actions/runs/41318041014)，对应 0810ca4e3d6de985a0a29f562c878d53f68d46a0；不代表当前整阶段新提交的 CI 已过。

本次桌面为 Cargo debug + Vite，未重新构建/安装 S01 NSIS 包，原交付测试包仍对应 S00。真实 CLI 发现/进程 stale 属 S02，历史/搜索属 S03，完整终端/生命周期属 S04，协议/权限/额度、最终安装升级卸载及性能仍 NOT_RUN。长会话详情目前完整返回，后续真实历史阶段需处理大内容边界，本阶段不作性能承诺。

下一步只交一张完整 S02 手动任务卡，不自动开始开发。
