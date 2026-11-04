# S07-R1 独立复验：源码通过，安装及最终性能门禁待完成

2026-11-04，实际完成返工 1 轮。S07-R1 修复范围通过；**S07 完整阶段尚未 ACCEPTED，不能写 Beta 完成**。以下初验的 REWORK_REQUIRED 为历史记录，本段覆盖其当前缺陷状态。

- 冻结 33 个开发回交文件并核对开发清单。主管小修只改备份实现和 bootstrap 回归；其他开发文件未变。小修：缺失快照显示 missing；list/plan/apply 的清单均先验证普通文件与 canonical 范围，再读取。补了 2 个备份正式回归、1 个完整运行时恢复重试回归。
- 最终 434 领域、96 前端、15 bootstrap、4 fixture 通过；原 5 反例、S06 10+2 反例、S05 11 批次均独立通过。S05 的 extra-boundaries 实际执行 6 项，没有零用例。私有 runner 引号错误及遗漏 S06_PYTHON 的失败均保留并正确重跑，没有改旧断言。
- F01：真实子进程提交 WAL 后不运行析构退出；完成记录失败时 WAL/SHM 原字节返回，事务行仍可读。正式 bootstrap 用损坏的隔离库验证：计划后同窗口的服务重试不提前应用，新运行时才应用。既有计划 id、标记删除失败不重放及新计划不误吞测试通过。
- F02/F03：原 65 PATH/4097 条目/未决入口反例通过；原 TRAE 去重用例在自有 8.3 短 TEMP 下独立通过。新 CI 尚未跑，不把本机结果冒充远端结果。
- F04：原目录外清单反例通过；计划/应用读取清单前也统一检查范围。实际 Windows symlink_file 夹具返回权限不足，**该项 NOT_RUN**，没有开启 Developer Mode、修改 ACL 或其他安全设置。私有测试输出的 4 passed 包括一个子进程入口和一个此项跳过，不能解读成 4 个新功能场景都已真机验证。
- A12：三核心各一条真实新建→拒绝（文件不存在）→允许（文件内容正确）→取消→停止→同 Session 新 Run 继续全部通过，版本分别 0.166.0 / 2.1.153 / 2.179.3；各有 8 项覆盖 PASS。原始正文/账号/DB 仅留私有证据，不进 Git。
- 发现延迟：默认 3 秒扫描，正常桌面 416 进程，20 个自有 Codex 空连接，p95 3076.20ms / max 5194.71ms，通过后台发现≤10秒目标；不含 React 绘制，不发送模型请求。
- 工具来源：Tauri CLI 2.12.3 的官方源码指定 NSIS3.13，官方 archive SHA1 `DB14F8AF2EA346B786C6AE19343F61AAF5E09B8E` 匹配，缓存 441 项与 archive 全部一致；NSIS.exe 与 nsis_tauri_utils.dll 的官方摘要均匹配。没有替换/安装工具。
- R1 回交原树的 All 四项通过；小修后的 Test 已通过。最终安装器还需从候选提交构建并单独记录二进制/安装器摘要，不使用初验或开发 final5 安装器。
- 后续门禁：A19 真实安装/升级/卸载、最终实窗/资源检查、8终端含 IPC/React/xterm 的可见回显 p95、最终远端/CI。开发 3.54ms 只测 TerminalManager，不足以宣称完整端到端达标。

本轮新证据：`scratch/S07/acceptance/supervisor-r1/`。本段记录源码验收，候选提交与安装结果由后续记录补充；无公开 Release。

---

# S07 独立初验：REWORK_REQUIRED

日期：2026-11-04（Asia/Shanghai）。主管 Sol，开发 DeepSeek。S00–S06 的既有验收保持；**S07 尚未通过，不能宣布 Beta 完成**。

## 基线与冻结

- base / HEAD：`6ef37fec0ca339f803504f073d9dfc5ff81819e9`，S07 没有提交、推送或 Release。
- 用户确认 DeepSeek 完工后冻结 29 个产品/文档文件；独立清单、逐字节副本和 patch 位于 `scratch/S07/acceptance/supervisor/submitted-*`。
- 回交：`docs/execution/deliveries/S07.md`。不把开发自测、旧阶段证据或零退出码当作本次独立通过。
- 本次主管没有修改产品实现、依赖、冻结 schema、原计划或 CI；新增独立探针、验收记录与 R1 卡。

## 独立验证

| 检查 | 结果 | 范围与证据 |
|---|---|---|
| 前端类型检查 / Vite 构建 | PASS | `verify-all.log`，含 4 项平台边界检查 |
| Rust `cargo check --locked` | PASS | 相同源码，原有上游警告未作为失败 |
| MultiCLI 前端 | PASS，96 项 | 11 个测试文件，实际发现并执行 |
| Rust 领域 | PASS，423 项 | 包括新增备份/用量自测；本地旧 TRAE 去重测试通过，不等于云端通过 |
| bootstrap | PASS，13 项 | 真实 IPC 测试边界，不等于真实三核心模型烟测 |
| fixture 保护 | PASS，4 项 | 原夹具保护保留 |
| 原 S06 原生反例 | PASS，10 项 | 独立重建生产源码，`retained-s06-native.log`；使用原断言的 S07 路径副本 |
| 原 S06 前端反例 | PASS，2 项 | `retained-s06-frontend.log`，原 F01/F02 |
| 新独立故障反例 | **FAIL，5 项全部失败** | `counterexamples-v2.log`；下面 F01–F04，共 4 个行为问题；保留先前日志 |
| 本地未签名安装包 | PASS，统一入口 Package 成功 | 从冻结未提交树重建，哈希见私有验证结果；不是已提交源码的最终交付物 |
| S05 私有完整反例 / 三核心真实重烟测 | 本次 NOT_RUN | 原开发日志已审阅；本轮发现阻断后留待修复树重新执行，不冒充主管独立重跑 |

原始证据根：`scratch/S07/acceptance/supervisor/`，不提交日志、数据库、真实账号资料或截图。

统一入口独立执行退出码 0，四个 scope 均 PASS。当前树重建的 exe SHA256 `0DC295BFE6285A9C41D212F83360694B6EFE5BD276D18DFF66AE29DD541F42D5`；安装器 6,642,657 字节，SHA256 `5E9FFF253A69B43E81739702E772EA6FC873A5D44B434A96607F12BDD3DA10EC`。两者 Authenticode 均为 NotSigned。它们与开发 final5 二进制有区别，不与前述 UI 检查混算；未安装、未签署、未公开发布，也不是最终已提交源码产物。

## 失败项与修复要求

### F01 / P1：恢复已替换当前库，却返回失败并宣称未替换

位置：`src-tauri/src/multicli/backup.rs::apply_pending_restore`，替换数据库后的完成记录持久化；以及 bootstrap 失败提示。

独立复现：创建真实 profile 和备份，随后增加一条 host；计划恢复，令 `last-restore.json` 成为不可写入文件的位置（用同名目录模拟完成元数据 I/O 失败）；关闭所有数据库连接后执行恢复。

实际：返回 `Err`，但备份后的 host 已被移除（预期保留 1 行，实际 0 行）。此时数据库已经换成备份，而 `mark_restore_failed` 会说“备份未应用，当前数据库保持不变”；pending 标记还保留。测试：`restore_reporting_failure_must_not_silently_replace_current_rows`。

必须修复完成提交点与错误语义：发生失败时可靠回滚数据库和 WAL/SHM，或者已完成恢复后持久化并准确报告已应用状态，不能报“原库未变”。处理标记移除失败、完成记录失败及重启重放；失败路径必须保留可恢复的旧库和未 checkpoint 事务。既有测试不能通过放宽断言、忽略错误或删除 pending 来变绿。

同时复核 `MultiCliRuntime::service()` 的错误重试：恢复必须符合“下次应用启动才执行”的契约，不能因数据页刷新重试服务而在当前窗口中提前执行。此项为静态风险，需增加原生回归验证，不作为已跑实窗结论。

### F02 / P1：有数量截断，却把扫描当成完整

位置：`discovery/detect.rs` 的 PATH 与每目录条目上限，及 S07 `usage/mod.rs::InstallationCoverage`。

两个独立失败：

- 传入 65 个存在的 PATH 目录，扫描只取 64 个，`truncated=false`。
- 目录中放入 4097 个条目，扫描只取 4096 个，`truncated=false`。

S07 据此计算 `conclusive=true`，缺失产品可被写成“已确认未安装”。生产 `ScannerEnvironment::from_process_env` 还会先把 PATH 截为 64，丢掉“确实存在更多目录”的证据。

测试：`path_limit_must_report_incomplete_scan`、`directory_entry_limit_must_report_incomplete_scan`。

必须保留上限防护，同时准确标记遗漏；检查根/条目枚举错误、上限边界和可确认产品的安装状态。不能通过无限扫描或启动同名不明文件来确认身份。

### F03 / P1：未确认入口被升级成“已确认未安装”

位置：`usage/mod.rs::InstallationCoverage::from_scan`。

独立复现：PATH 中有一个不能证明产品身份的 `codex.exe` 合成入口。生产扫描正确返回 `Unverified`，但用量目录把它映射成 `NotInstalled`，而不是 `Undiscovered`。

测试：`an_unverified_executable_is_not_proof_that_the_product_is_uninstalled`。

必须按产品保留未知状态：已证实可运行入口为 installed；有未决候选/不可完整读取为 undiscovered；扫描完整且确实无可疑候选才可 not-installed。已有不可运行残留和确定缺失目标应按证据区分，不能全部合并成未知或安装。保持普通刷新退避、不自动运行未确认文件、不猜测身份。

### F04 / P2：备份列表在校验路径前读取备份目录外文件

位置：`backup.rs::list_backups`。

独立复现：复制一份真实快照到 profile 下但在 `backups/` 外，清单中写 `fileName=../outside.db` 并使用相应真实校验信息。列表返回 `integrity=ok`，已打开并计算目录外文件的校验和。`plan_restore` 的文件名校验没有保护列表读取。

测试：`backup_list_must_not_validate_files_outside_own_backup_directory`。

必须在 list/verify/plan/apply 等读取入口统一落实本 profile 范围；非法清单保持明确不可读/不可用，不能静默消失、读取任意绝对路径或显示可恢复。补绝对路径、盘符路径、分隔符和 Windows 目录链接/重解析点边界；不要求通过修改系统安全配置造环境。

### D01 / P2：交付文档需与实际状态、恢复边界一致

- `README-MultiCLI.md` 一面写 S07 SUBMITTED，一面把备份/安装交付写成“已交付（S07）”。目前仍待验收，应统一。
- GitHub 默认 `README.md` 顶部仍说“S00 基线、产品尚未实施”，需要更新入口状态；保留固定上游正文与归属。
- `QUICKSTART.md` 的手动恢复步骤应先保存当前 DB/WAL/SHM，校验目标快照与 schema，再替换；不能只指导覆盖库并删除 sidecar，缺少失败恢复路径。
- 真实账户的掩码别名/套餐不应出现在拟提交交付文档；用“一个已确认账户池”概括，细节留私有证据。
- 安装工具镜像来源、版本与完整性证明需提供给主管，不能把“镜像可下载/打包成功”当作官方产物校验。

## 真实窗口、数据保护与安装范围

主管通过 Computer Use 操作自有 release 副本，二进制 SHA256 `889B359C08F05EF1B457501DA26FD318B6AD3F82956EB06A02404856AFB6E99F`，与开发 final5 产物相同。它早于最终提交树，**只作为该二进制的界面检查**，不充当最终受测源码安装验收。

- 数据根为 `scratch/S07/acceptance/supervisor/ui-profile`，profile=test，不使用用户现有 profile。
- 首次启动：无演示工作区/会话，空态有启用历史来源的导航。
- 数据与备份页：显示正确的独立数据根；实际点击立即备份生成 SQLite 文件和清单，界面显示 schema 7、校验通过。
- 未打开安装页前访问用量页：Codex/Claude/CodeBuddy/TRAE 显示已安装；未进行模型推理或额度刷新。
- 从应用自身“退出”正常退出，已确认窗口消失；未结束外部进程。
- `ui-observations.json` 为文本观察记录。没有通过 UI 输入终端命令，没有改变全局 CLI 配置、登录、PATH 或安全设置。

原真实 test profile：34 会话 / 11 消息。使用当前 Node SQLite 只读连接生成快照与每表行摘要，验收后逐行摘要一致；dev/prod 原来不存在，仍不存在。`profile-native-before.json` / `profile-native-after.json`。最初受限环境不可见数据，以及旧 Python SQLite 不支持 STRICT 的失败均保留，不能当成“用户无数据”的证据。

本轮没有真实安装/升级/卸载、没有改 Hyper-V/网络、没有删除 target，也没有发布 Release。

## A01–A20 最终门禁状态

| 范围 | 当前独立结论 |
|---|---|
| A01–A02 导航/身份 | 正式回归通过；首次空态实窗已核对，完整流程保留原阶段范围 |
| A03–A05 外部发现/身份 | 原阶段证据保留；本次正式回归通过；外部发现 p95 本次 NOT_RUN，F02 待修 |
| A06–A08 历史/搜索 | 原阶段证据保留；没有 S07 新的 10k/100k UI 端到端测量 |
| A09–A11 终端/托盘/参数 | 原 S04 用户真机证据保留；新窗口正常退出已核对，IME/托盘及端到端负载本次未重复 |
| A12–A14 三核心/竞态/继续 | 正式相关测试通过；三核心新建/继续/取消/权限真实重烟测仍 NOT_RUN，修复树再测 |
| A15–A16 额度/费用 | 原 S06 10+2 反例与正式新增边界通过；目录未知/未安装仍受 F02/F03 阻断 |
| A17 覆盖真实性 | 原 S06 两款扩展有限真实证据保留；无新增安装或全版本承诺 |
| A18 本地/隐私 | 用户数据逐行保留；备份范围 F04 待修，拟提交文档脱敏待修 |
| A19 安装/恢复 | F01 阻断；最终已提交源码安装包、真实安装/升级/卸载、干净环境检查均未完成 |
| A20 未实现状态 | F02/F03 阻断；不能因已有前端测试绿灯写通过 |

开发空闲 5 分钟、8×100 KiB/s 与 20 轮报告已审阅；它们属于开发测量，后者不含 Tauri IPC/React/xterm，不可用其 3.54 ms 直接宣称“可见输入回显 p95 达标”。主管最终需补外部发现 p95 与终端端到端测量/明确覆盖界限；原性能目标不放宽。

## CI、返工与下一步

- 2026-11-04 重新读取 base 的 GitHub run `41662675755`：completed / failure。此前 S06 状态文档中的 in_progress 是旧快照。
- 已知失败是 TRAE native-shim 去重，本机正式测试通过但远端失败。R1 需保留原断言，核对 Windows 临时路径别名、入口/残留合并及大小写；有本地复现再修复。DeepSeek 不操作 GitHub，最终云端验证由 Sol 在验收提交后执行。
- S07 初验 **REWORK_REQUIRED**，实际完成返工 0 轮；一张 `dispatch/S07-R1-DeepSeek.md` 已准备、尚未派发。涉及数据恢复提交点与跨模块扫描语义，属于成组返工，不按零星文案小修处理。
- 用户手动转交后才开工，完成停写回交；最多实际 R1/R2，两轮后主管收尾。不启动 Baton、不新增 Agent。
- 修复后继续三核心真实重烟测、性能和真实安装/升级/卸载，从已提交源码构建最终安装器，记录 SHA256、远端与 CI。安装器生成不等于最终验收；未单独授权不发布 GitHub Release。
