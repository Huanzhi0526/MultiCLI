# S02 独立验收记录

2026-11-02，Sol。**ACCEPTED（独立验收通过）。提交/推送另行核实，准确 SHA 见根 STATUS 与 Git HEAD/origin/main。**

## 基线与角色

base_commit 为 efcbfa89ecd006ba1117a43195408132d97cbed5。DeepSeek 整阶段回交已冻结，原回交见 deliveries/S02.md。配置按用户确认 WorkBuddy / deepseek flash 1017 / 1M / max；宿主回交显示 DeepSeek-V4.1-Flash，不猜后台 API ID。

按用户“少量问题直接修或用 Luna、不再发小返工卡”的授权，本次由 Sol 直接补识别、身份、脱敏、IPC/界面状态边界，Luna（gpt-6-luna）仅修版本探针的管道回收。没有 Baton，没有新增主开发团队；开发侧没有 Git 写入，验收后由 Sol 提交。Luna 强度/上下文未额外指定，不假填。

## 已独立执行的验证

| 验证 | 实际结果 |
|---|---|
| Frontend / 构建边界 | 构建 PASS，边界 4/4 |
| 前端 Vitest | 30/30 PASS，包含同一扫描代次完成状态回归；组件 mock 只作单元层证据 |
| 最终实际原生 crate 测试 | 107/107 PASS，含 96 领域/发现及 11 启动/IPC/真实 ConPTY |
| 夹具保护 | 4/4 PASS；本批没有改夹具源码 |
| 主管独立模块重编译 | 101/101 PASS：相同实际源码的 96 项领域/发现 + 5 项主管反例，无复制业务实现；受控后端只用于竞争和拒绝访问反例 |
| 原生 locked 检查和构建 | PASS；最终 debug 二进制已构建。保留 499 条上游及 5 条非阻断告警，不宣称零告警 |
| v2→v3 独立迁移 | 用 S01 真实合成库副本加备注/标签，成功升级后原逻辑记录/引用全保留；故意制造中途 SQL 冲突后回滚到 v2，安装表无半迁移列，外键无异常 |
| 继承管道超时反例 | 原来约 3.34 秒返回，超过 1.2 秒期限；修后主管复验约 271 ms，仍读到版本 0.0.1；停止后持续输出的上限回归通过，不杀后代或留下阻塞 reader |
| A03 无界面真实后端 | 真实 WindowsProcessBackend + 实际检测/存储引擎，独立 scratch 数据根；真实 ConPTY→PowerShell 启动三核心，未发送输入/prompt、未登录账号。Codex 3238 ms、Claude 3247 ms、CodeBuddy 3187 ms 被发现，每款本轮 1 个长存活样本，不冒充 p95 |
| A04 真实父子/负例 | CodeBuddy 为 PowerShell→cmd→Node，来源程序 cmd.exe；Codex/Claude 为 pwsh.exe。普通 node 的产品名文本参数、PATH 中改名为 codex.exe 的普通 Node 均不误报；观察到正确 alive→exited 收敛 |
| 身份与脱敏 | 外部 OS 创建时间按完整 100 ns 身份核对；真实视图/SQLite 无本次私密标记，未知工作区不误绑；未建立虚假的 Session/Run/协议连接 |
| 数据与进程收尾 | 本次控制的 CLI、ConPTY 宿主与独立扫描器均已结束；两份宿主候选 test 库的 10 张非发现业务表与备份逻辑一致，外键无异常，所核对 5 个旧配置/数据路径摘要不变。不是对整个 CLI 数据树的穷尽证明 |

上述无界面真实链路与下面补齐的实际 Tauri 界面按钮验收分别记录。初次测试启动器引号错误、以及没有保持交互终端的短命 CLI 现场保留，未计为长存活正例；后改用真实 ConPTY 才取得上述结果。

## 已补齐的少量缺口

- argv[0] 可伪造，曾把普通镜像识别为 Codex；现在只信 OS 镜像。
- 枚举后补查发生 PID/镜像竞争，旧识别会贴到新进程；现在复验补查后的产品/目录，OS 后端补查前后核对创建时间，父来源也使用可核对的父身份。
- 本轮失去识别却仍显示 alive；现在 unknown/stale，不造退出或活跃结论。整次枚举失败保留未确认候选。
- PATH 中同名普通 exe 曾成为可信安装并进入探针；现在仅作 unverified 候选，不探测、不作为确认实例；可信物理安装根/实际包入口仍可识别。
- 子进程退出不意味着继承管道已关闭；Luna 改为轮询本次自有管道、保留 8 KiB 上限并有界回收线程，Sol 独立复验。
- argv[0] 本身也可能包含私密文本；现由 OS 镜像名生成脱敏首字段，失败候选和成功观察均走同一路径。
- 同一 generation 内 scanning→完成也必须更新 UI，不能只比较代次号。
- S01 的 GitHub CI 41330890875 实际 failure：Windows temp 路径别名使 diagnostics containment 的字符串比较失败。已改为真实 canonical 路径包含校验，保留断言；旧远端 run 仍是 failure，不声称已绿。

主管接受有界扫描设置所必需的第四个 IPC；只写本 profile 设置。安装 ID 继续按卡中的真实 canonical executable/package 身份，版本数值不参与 ID；升级若切换到另一物理目标会形成另一身份，真实升级仍未测，未擅自改为入口路径合并。

## 解锁后的实际 Windows 界面验收

用户解锁后，通过 computer-use 的真实 Windows 输入和可访问性/截图独立操作最终受测程序，未用 CDP 或组件 mock 代替产品按钮。

| 验证 | 实际结果 |
|---|---|
| 安装页/立即刷新 | 核心三款 installed 与版本正确；额外未知/残留候选分开；当前筛选 6 条，多次刷新不重复。不是全九族安装扫描证明 |
| 实际进程页 | 真实 ConPTY→PowerShell 启动三核心，UI 自动出现 3 个 fresh/alive 实例，完整 100 ns 身份与独立 OS 核对一致；普通 Node、PATH 同名假 Codex 不出现；未知工作区保持未关联 |
| 第二组实际发现延迟 | Codex 947 ms、Claude 967 ms、CodeBuddy 881 ms；以 OS 创建时间至后端 firstSeenAt 计算，毫秒取整。与前组共每款 2 个长存活样本，不声称性能 p95 达标 |
| 整次枚举失败/重试/恢复 | debug 故障注入后旧 3 行保留为 stale，最近成功时间不变；实际点击重试仍失败时不伪造成功；清除故障后同身份恢复 fresh |
| 部分补查拒绝/恢复 | debug 注入后已知条目 unknown/stale，加创建时间不可读的未确认候选；清除后收敛为原 3 个 fresh 身份。拒绝路径是注入，不冒充真实受保护系统进程 |
| 扫描设置 | 真实选择 5 秒并读取本 profile 设置确认 5000；再恢复 3 秒，读取确认 3000 |
| S01 实窗回归 | Alpha、Beta、Agent 三入口都打开同一个 fixture-session-cross，6 条消息/3 个 Run 的详情文本摘要完全相同；实际刷新仍保留详情 |
| 实际诊断导出 | 点击按钮导出 schema v1 / database v3；独立检查顶层白名单、匿名 Agent、无路径/native id/命令/正文，10 项计数与实际 SQLite 一致 |
| 正常关闭窗口 | Alt+F4 后应用退出约 196 ms，自有 WebView 为 0；立即按 PID+完整创建时间核对，三 CLI 和两个负例均仍活着。随后仅本轮测试控制器停止自己的进程树，Vite 也已按命令行/创建时间核对后停止 |
| 数据保护 | 最终再次核对两份候选 profile：各 10 张业务表无差异、外键无异常，抽查的 5 个旧工具/其他 profile 路径摘要不变；不宣称穷尽检查所有用户数据 |

原锁屏阻碍已解除，以上阶段范围无阻断缺陷。A03/A04/A05/A20 在表列边界内通过；PID 复用/父退出/竞争由确定性反例覆盖，真实进程与 UI 另外覆盖，不混淆证据层级。旧 S00 安装包未更新，真实历史、协议/权限、额度、托盘/保活及最终发布验收仍属后续范围。真实升级、跨用户/受保护进程、长时性能 p95 未测。

## 证据与恢复入口

原始证据统一 scratch/S02/acceptance：submitted-source.json/patch、final-tested-source.json/patch、native-tested-final.log、all-domain-and-probes-final.log、frontend-final.log、test-final.log、independent-migration.json、adversarial-before/trust-before/after、argv0-privacy-before.log、timeout-final.log、real-discovery-assertions.json、headless-real-final/snapshot-*.json、profile-*-before/after.json、controlled-processes-after.json 、ui-lock-blocker.json、ui-*.json、ui-independent-check.json 与 ui-live/app-exit-boundary.json。均不提交原始数据库、日志或产物。

受测产品文件 1507 个，当前工作树与摘要一致。实窗已验二进制 SHA256 为 80EFB5018602F742B095710D3967679BD9B852561B87C320620B3190C3AB3E54。1507 个受测产品文件经独立摘要核对保持一致；提交前后再核对 Git blob。后续只准备一张完整 S03 手动卡，不自动派发或恢复心跳。
