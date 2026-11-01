> 历史首轮验收：当时 REWORK_REQUIRED，原记录保留。数据层已经修复并通过；当前结论见 [S01 阶段验收](S01-ACCEPTANCE.md)。

# S01-01 独立验收（首轮）

2026-11-01，Sol。结果：**REWORK_REQUIRED，返工 S01-R1**。没有 commit/push，不进入 S01-02。

## 基线、范围与模型记录

- base_commit / 当前 HEAD：`c3db27a8158bb0fef03a82dfafb74a7302250a58`，未变；DeepSeek 未提交。
- 回交仅改 lib.rs 的一行模块声明，新增 multicli 下 7 个 Rust 文件与 S01-01 回交文件，仓库内未越出派工范围。Rust/前端依赖、冻结契约、启动入口没有变化。
- 用户在本聊天确认的实际宿主配置：**WorkBuddy / deepseek flash 1017 / 1M 上下文 / max**。这是用户确认的宿主显示标识，不把开发者自报的 `DeepSeek-V4.1-Flash` 当成已核实的后台 API ID。回交正文须据此补正。
- 验收前保存 1585 个文件的完整摘要；全部测试及 SQL 复核后，源码差异为 0。下面主管记录是在冻结检查之后新增；业务代码未由主管修复。

## 独立执行结果

| 验证 | 实际结果 |
|---|---|
| cargo.exe check --locked | PASS；499 条保留旧模块告警，不执行自动修复 |
| 原开发测试由 Sol 重跑 | 26/26 PASS，0 ignored；不是仅参考自测报告 |
| S00 multicli_bootstrap 回归 | 3/3 PASS，0 ignored；通过真实 Windows PTY 的原生测试 |
| 主管单独反例探针 | 9 个执行，1 PASS、8 FAIL；首次及重新准备干净 junction 的复现结果一致 |
| 主管正例 | 三 Agent、两工作区、跨工作区同会话/同计数查询 PASS；阻断备份目录后，升级失败仍保留 v1 与消息 PASS |
| 开发迁移现场独立 SQL 读取 | 失败主库与备份均 v1、恢复库 v2；三个文件 integrity_check=ok，备注与标签仍在 |
| 主管反例数据库独立 SQL 读取 | 跨会话/Agent/Host 与外部 owned 错误关系已经实际落库；foreign_key_check 仍为空，说明仅有单列 FK 不足以守住领域归属 |
| 新源码桌面、完整 IPC、安装包 | NOT_RUN；数据层阻断已明确，不以旧 S00 二进制冒充本次运行验证 |

主管探针在 scratch/S01-01/acceptance/probe，通过真实绝对路径编译本次 multicli/mod.rs 及其子模块，不复制 store/repo/service 实现，不使用 mock。合成库、备份、junction、日志均只位于项目 scratch；没有打开真实历史或旧工具数据。外部 SQL 复核使用 Node SQLite 3.53.0，只读打开合成数据库。

## 缺陷与实际重现

| ID / 严重度 | 实际结果 | 预期及修复范围 |
|---|---|---|
| R1-01 / P1 领域引用归属 | Message(s-a, runId=r-b) 可成功落库，r-b 属 s-b；agent-a/host-a 的 Run 可引用 agent-b/host-b 的安装；agent-a 的能力可引用 agent-b 的安装 | 真实写入入口必须校验同 Session、Agent、Host 归属并原子拒绝无效输入，不能只检查被引用 id 存在。repo.rs / service.rs / 必要 SQL 约束；对应 3 个失败探针 |
| R1-02 / P1 外部进程所有权 | external-observed Run 首次 link_process(linkKind=owned) 返回 Ok 并写入 owned | 外部观察不能取得拥有/停止语义；拒绝非法关联且不改 Run 或现有关系，合法 observed/inferred 保留。repo.rs / service.rs；1 个失败探针 |
| R1-03 / P2 安装身份冲突 | 已登记 same-id=agent-a/host-a，再登记同 id 的 agent-b/host-b，返回 Ok(agent-a/host-a) | 不得把不同安装身份当成功幂等；明确 conflict/invalid_argument，合法同身份仍可幂等。repo.rs；1 个失败探针 |
| R1-04 / P1 数据根隔离 | 合成 alias junction 指向合成 .ccgui-next，open(alias) 成功且在受保护目标创建 multicli.db | 在任何创建/写库之前验证实际路径及已有父级重解析点，不让词法别名绕过旧目录拒绝。保留显式合成测试根支持；不把本机路径硬编码进模块。store.rs；1 个失败探针 |
| R1-05 / P2 拒绝较新数据库仍写入 | 版本 999、journal_mode=DELETE 的合成库被拒绝，但文件字节已改变，journal_mode 变成 WAL | 对不支持版本先只读检查，在拒绝前不改持久化 PRAGMA、schema 或文件；不是数据丢失结论。当前 open_inner 在 migrate 版本检查前设置 WAL，store.rs；1 个失败探针 |
| R1-06 / P2 冻结 wire required 语义 | HostPath 缺 canonicalPath 键仍可 deserialize + validate，自动变 None；冻结 schema 要求该键存在，值可以 null | 按冻结 required 约束区分“缺键”与“明确 null”；审查同类 nullable 必填 DTO，不能修改 schema 使测试变绿。domain.rs；1 个失败探针 |

代码位置：repo.rs 的 upsert_installation:92、insert_run:385、link_run_process:528、upsert_capability:654、insert_message:779；store.rs 的 open_inner:312、validated_root:369、migrate:424；domain.rs 的 HostPath:72。位置以本次回交摘要为准，返工后可能变化。

## 复现与证据

在项目根的 PowerShell 7 中执行：

```powershell
./scratch/S01-01/acceptance/run-adversarial.ps1
```

脚本为每次运行创建新的合成 junction，调用已锁定离线探针；当前退出 101、1 通过/8 失败。修复后要求 9/9 PASS，且必须补入正式模块的有效回归测试；不能只改主管探针或排除失败用例。

证据仅在项目根 `scratch/S01-01/acceptance/`：submitted-source.json、freeze-check.json、model-parameters.json、cargo-check.log、developer-tests-independent-run.log、bootstrap-regression.log、adversarial-tests.log、adversarial-r1-repro.log、independent-sql-inspection.json、data/ 与 junction-*。失败数据库和原始日志保留。

## 流程补正与下一步

回交披露在允许目录之外新建 scratch/tools/cargo/bin/cargo.cmd，通过 PATHEXT 优先选包装器规避 PowerShell 5.1 stderr 行为。Sol 的真实验收使用 PowerShell 7 与绝对 cargo.exe，不需要该包装器。返工需把本批自建包装移入允许的 scratch/S01-01/tools 并留证，不能修改或删除 cargo.exe、Rust 工具链或全局 CLI。原包装 SHA256：5E121B02EA0131EE85C72F9355214457739A3A7F06AF3DDCFDA7166512644CDA。

按 [S01-R1](rework/S01-R1.md) 修复当前数据层，用户手动带回 SUBMITTED 后重新冻结并验收。模型参数已由用户补足，不因该记录重复阻断施工。S01-02 与后续阶段保持 NOT_RUN；CI、真实产品历史/权限/额度验收没有新增 PASS。
