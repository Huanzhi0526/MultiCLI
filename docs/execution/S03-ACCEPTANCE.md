# S03 独立验收记录

## S03-R2 独立复验与主管收尾（2026-11-02）

**ACCEPTED（S03 范围）**。本节为当前结论；下面 R1/首轮材料保留为历史，不表示旧缺陷仍存在。

base_commit `a24ba3befce1947d4b7f4858c8a4c0c5e01d7a4c`。回交后冻结，实际模型 WorkBuddy / deepseek flash 1017 / 1M / max；没有 Baton/新增 Agent。用户本次授权剩余 S03 问题由 Sol 直接修，未再次派返工卡。

独立结果：原生正式测试 198 领域/发现/历史、11 bootstrap/IPC/真实 ConPTY、4 fixture，39 前端、4 平台边界，前端构建与原生实际二进制构建通过；主管 SQL 修复后 199 领域/发现/历史（含 103 历史）、11 bootstrap、4 fixture 全部通过；独立原 12 反例再次通过。

原 12 个独立反例与 R1 新增 3 个反例共 15/15 通过，直接编译调用生产扫描/查询模块，无复制解析业务、无 mock 历史后端。持续追加的旧前缀复核、合法大 zstd 物理块、Full 重读检查点均已独立验证，原失败断言未放宽。v3→v6 普通用户真实 profile 升级后，原有会话/消息/安装/能力/备注标签各原始行完全保留；FK 检查无错误。正式迁移成功/失败回滚用例一并通过。

真实历史只读：四个当轮原始文件经 SHA256 校验后复制到 scratch，三核心合计 4 会话/136 消息；首次导入及关闭重开后，完整正文、时间线顺序、稳定身份逐条摘要相等。随后仅对隔离副本追加 1 条再等长替换，137 条无重复；原四文件始终不变。等长替换首次保持 mtime 的检查在 15 分钟复核冷却内，保留未更新结果，不宣称瞬时发现；修改副本 mtime 后立即复核成功，同 size/mtime 的有界最终恢复另由独立合成反例验证。实际 HistoryEngine 重建三个来源后，137 条完整正文/顺序/ID/sourceKey 及自有备注/标签全部保持。工具原有 message_digest 只含正文长度，本轮另做完整正文摘要，不用长度摘要冒充全文一致。

普通用户 Windows 实窗：实际表单启用唯一选定的合成 Codex 来源；135 条历史，无伪造 Run/协议。中文长词 FTS、两字 LIKE、点击结果定位并高亮 #123 通过。来源目录暂不可用时 stale/error 保留缓存及最近成功时间，实际重试恢复。Alpha 主目录、Beta 关联目录、Codex Agent 三入口同 sessionId、前 100 条正文/顺序相等，加载更多达 135 条且无重复。实际继续检查为 unknown，执行未开放，不冒称 S05 已完成。实际诊断导出 schema6 白名单计数，不含正文/原生会话 ID/CLI 路径/凭据。普通 Alt+F4 退出后应用/WebView 进程均停止；原 CLI 配置及 dev/prod DB 摘要不变。

主管收尾修改：保留此前 Full 检查点与 S02 TEMP 物理路径回归；本轮修基准 --reuse 重复注册工作区冲突，并修搜索计数/分页重复扫描过滤匹配。保留最初 fresh 基准（最慢 p95 913ms）与 reuse 复跑超标（1160ms）原结果，最终成绩来自修后独立复跑，不择优隐藏失败。

性能条件：release 生产查询，10,000 合成会话/100,000 消息，Windows x86_64 本机；暖缓存 3 预热、30 样本 nearest-rank p95；计同步 SQL/结果映射，不含 Tauri IPC/React 绘制。初次真实索引 10pass/214.706s、0行错误、10000游标、100000消息、227041280B 库。每个宽泛过滤路径均命中100000、只返50；最终最慢 searchEnglishFtsBySource p95 **623.752ms**，全部 p95≤1秒。不把该结果等同所有真实历史规模或 UI 端到端耗时。

限制：JSONL 预算分轮；压缩物理128MiB/解压256MiB上限、单文件1.5秒事务需足够磁盘吞吐，拒绝/超限/未完成可见。30秒补扫，无文件监听；完整性复核15分钟冷却并随总量分轮，非即时监听保证。每轮改写头部的>32MiB文件可反复 Full，限制明确；同数据根多实例并发不是已验收路径。UI 重建的最终确认按钮未点击，生产 HistoryEngine 重建通过隔离真实副本验证。S03 未更新/验收发布安装包；托管终端/托盘、真实协议权限/恢复执行、额度和最终安装交付为 S04–S07 NOT_RUN。

证据只留根 scratch/S03/acceptance/r2：tests/frontend/native-build、两组独立反例日志、真实源基线和全正文比较/重建、ui-*.json 与导航比对、profile-after、基准失败及修后 JSON、最终源码/二进制摘要与 publication audit。原始历史/私有路径/日志/DB/target 不进 Git。Git 提交、远端 SHA、新 CI 独立核实，不把旧失败 CI 说成成功。

修后各路径（ms）：

| 路径 | p95 |
|---|---|
| listSessions | 2.832 |
| messagePage | 0.218 |
| searchCjkFtsPage2 | 409.207 |
| searchCjkFtsUnfiltered | 398.845 |
| searchCjkFtsByAgent | 280.084 |
| searchCjkFtsByWorkspace | 273.39 |
| searchCjkFtsBySource | 600.149 |
| searchCjkShortLikeUnfiltered | 175.775 |
| searchCjkShortLikeByAgent | 223.257 |
| searchCjkShortLikeByWorkspace | 209.999 |
| searchCjkShortLikeBySource | 500.017 |
| searchEnglishFtsUnfiltered | 428.607 |
| searchEnglishFtsByAgent | 313.886 |
| searchEnglishFtsByWorkspace | 312.944 |
| searchEnglishFtsBySource | 623.752 |
| searchPercentLiteralUnfiltered | 398.797 |

---


以下为当时失败记录；当前状态以上方 S03-R2 为准。

## S03-R1 独立复验（历史结论，2026-11-02）

**当前仍为 REWORK_REQUIRED，下一张卡 S03-R2。** S03-R1 修复有效：上轮12个独立反例已全部通过；原回交正式测试185领域/发现/历史、11bootstrap、4fixture、39前端也由Sol重跑通过。以下不是重复首轮问题，而是对R1完整性方案的进一步复验。

新增直接编译实际模块的3个合成反例，原回交3/3失败：

| 项目 | 独立结果 | 当前处理 |
|---|---|---|
| 持续追加时的旧块复核 | 约2.6MB初始文件，中间等长改写后每pass追加约130KB，共15轮；新消息进入，旧中间改写一直未入索引。decide始终Append，verifyOffset虽保留但verify_window从未执行 | P1，S03-R2-01；需对活跃文件的已提交前缀给出真正可推进的复核，不能要求停止写入 |
| 大压缩文件完整性 | 合法zstd解压后约70MB/350消息，已完整导入；中间等长改写、重新编码并保持物理大小和mtime。10轮均complete=true、pending_verification=1、verified_bytes=0，正文仍旧。logical>64MiB时源码无条件Deferred | P1，S03-R2-02；需要可推进的有界复核，不是提高阈值或永久等待 |
| Full重读复用旧parser检查点 | 替换首条用户消息后正文更新，派生标题仍OLD_TITLE；Full仍restore旧title_seen/cwd等状态 | **主管已按小修授权修好**：Full默认检查点，仅Append恢复。正式新增回归和独立同类反例2/2通过 |

压缩反例为有效含checksum和合法skippable frame的zstd流，没有伪造数据库或破坏文件；文件完整导入后才测试复核。新增案例只是使用合成内容，不读取用户凭据。持续追加反例使用真实生产扫描器与SQLite，无mock历史后端。

主管小修后全部90项历史正式回归与原12项独立反例再次通过；前端构建和4项构建边界也独立通过。新增3项反例最终复验为标题1项通过、上述2项仍失败（additional-after-small-fix.log）。此处185/39等为修前原回交全套结果，90为主管小修后的历史定向回归，不冒充全套原生重新验收。

代码/证据保存在项目根scratch/S03/acceptance/r1：submitted-source.json/patch、test-submitted.log、original-counterexamples.log、additional-counterexamples.log、full-checkpoint-fixed.log、history-after-small-fix.log、original-counterexamples-after-small-fix.log、probe/和cases/。原probe的12项断言未修改。主管本轮只改history/scan.rs的Full检查点及history/tests.rs增加回归，没有新增Agent。

真实三核心副本独立比对、实际Windows界面/滚动/导入中响应/退出，以及独立10k/100k性能复验仍NOT_RUN，待核心阻断解除后执行。开发回交的真实副本/性能与自行审查仅属开发自测，不替代这些独立验收。未构建/发布S03安装包，不commit/push，不派S04；用户手动转交一张完整S03-R2卡。

## 首轮验收历史

**以下保留首轮当时事实。原12失败反例现在已经通过；当前待办以上方R1复验和S03-R2为准。**

## 基线与范围

base_commit：`a24ba3befce1947d4b7f4858c8a4c0c5e01d7a4c`。开发回交见 deliveries/S03.md，原回交与 1529 个产品文件的摘要已保存在项目根 scratch/S03/acceptance/submitted-source.json、submitted.patch。回交后无开发并发写入，未 reset/清理用户修改。

实际配置沿用用户确认的 WorkBuddy / deepseek flash 1017 / 1M / max，宿主显示名另记 DeepSeek-V4.1-Flash。没有 Baton，没有新增 Agent。用户允许小修直接处理，但下面的索引完整性、来源追踪与分页缺口涉及核心实现，已合并为一张 S03-R1，不以主管代写整块业务替代开发返工。

## 已执行的独立验证

| 检查 | 实际结果 |
|---|---|
| 前端构建/构建边界 | PASS；边界 4/4 |
| 原回交前端测试 | 37/37 PASS |
| 原回交 Windows 原生测试 | 146 项领域/发现/历史 +11 项启动/IPC/真实 ConPTY +4 项 fixture PASS |
| 受限执行环境首次测试 | 两项创建 junction 测试失败；转真实 Windows 用户环境后保留断言重跑全部通过，不把环境限制当产品缺陷 |
| 主管独立反例 | 直接编译并调用实际 multicli 模块，无复制索引业务、无 mock 历史后端；第一组 11/11 失败，另一个默认预算反例 1/1 失败，详见下表 |
| 源码/范围 | 原业务与冻结摘要一致；主管只额外修一处 S02 测试夹具的物理路径别名，不改 S03 业务 |
| Git | HEAD 保持基线；本轮不暂存、commit、push，不派 S04 |

上表的绿色测试不能覆盖下表的失败。原始日志、合成失败数据库与反例源码只保存在 scratch，不发布真实历史或大型产物。

## 已确认的缺陷

| 编号 | 级别 | 重现与实际结果 | 预期 / 涉及实现 |
|---|---|---|---|
| R1-01 | P1 | 同一文件有 12 条消息，每文件预算设 400 B，首 pass 导入 2 条并报 complete=true；重复 11 个 pass 仍仅 2 条。普通 JSONL 与 zstd 都复现。另用产品默认 32 MiB/4 秒，约 38 MB 合成文件只导入 140/160 条却 complete=true | 未到 EOF 必须 complete=false，下一 pass 继续；scan.rs::decide/process_file/stream_file/ScanReport；压缩流的物理与逻辑 offset 不能混用 |
| R1-02 | P1 | 同一 UUID 的无原生 event id/ordinal 消息跨多次追加，每条超过 4 KiB；4 条最终仅 2 条。改名为 renamed.jsonl 的有效 Codex 文件追加后，从 1 个 Session 变成 2 个 | 保存或恢复 parser 元数据、稳定行号/offset、原生会话身份；不得靠文件名重新猜头部身份，fallback key 不能每 pass 从 1 开始 |
| R1-03 | P1 | 同一来源两个文件提供相同的 2 条消息；截断后处理的副本为零，再扫描后消息从 2 变 0，原文件仍完整存在 | origins 必须保留同一来源的所有文件贡献，清理仅删除再无贡献者的派生消息。store.rs 的 PRIMARY KEY(message_id,source_id) 与 repo.rs 的覆盖式 upsert/清理丢失多文件来源 |
| R1-04 | P1 | 被允许的来源根内放合成 .ssh 子目录，其中 2 条有效 JSONL 消息被正常导入 | 根校验的受保护目录策略应同样作用于递归子目录；保持 reparse/边界防护，禁止读取实际凭据。反例只用合成文件，没有访问用户 .ssh |
| R1-05 | P1 | 两个超过 4 KiB 的边界窗口之间改写中间消息，保持长度与 mtime，导入内容保持旧值 | 必须有有界的完整性复核/文件身份与内容验证，最终发现中间改写；不能把硬要求改成只验证首尾。Compressed skip 同样不能只信 size/mtime |
| R1-06 | P1 | 坏 JSON 位于第一行、后面是有效 header+2 消息时，整文件拒绝，0 条导入；坏行位于有效 header 后时，索引 status=ok、error=null、complete=true | 有界格式识别后隔离坏行并继续；行错误/拒绝/超限必须传播到真实状态、位置与错误摘要，不伪造全成功 |
| R1-07 | P2 | 搜索 A 的 cursor 改用于另一关键字仍被接受；Session A 消息 cursor 用于 Session B 仍被接受 | 游标绑定规范化查询/过滤、session、排序与数据版本，错用明确失败，不静默跳页；model/query 与前端防晚到响应一起回归 |

主管反例命名、源码与完整断言见 scratch/S03/acceptance/probe/src/lib.rs；这是对生产模块的独立测试，不是另一份解析器。缩小预算只为快速暴露同一分支，默认预算反例另外确认了实际配置下的未读数据与错误完成状态。

## 额外审查与待补验证

- SessionBrowser.tsx 仍通过 api.getSession 加载全量消息；历史页虽分页，工作区/Agent 入口也能打开这些历史会话。需让大历史所有入口走有界元数据/消息链路，再验同会话一致性与长会话内存/分页。
- 开发回交自报：带 Agent/Workspace/Source 过滤的宽泛搜索约 1.3 秒，未满足原卡暖缓存 p95≤1秒目标。主管本轮未独立重跑大规模性能；返工须完整测过滤和未过滤路径，不能只优化无过滤后宣布全部达标。
- HISTORY-FORMATS.md 的真实副本核对段包含来自真实会话的查询原文；公开资料必须改成合成示例/匿名统计，真实文本留 scratch。
- 真实三核心副本的开发证据仍属开发自测；主管真实源只读核对、正文与顺序比对、实际 Windows 界面/滚动定位、导入中响应、关闭/重启等 **NOT_RUN**，待阻断修复后执行。不把 jsdom 当实窗，不声称已通过 A06/A07/A08/A14/A20。
- 尚未进行 S03 发布安装包验收；旧 S00 包不代表 S03 通过。

## 已直接完成的小修与旧 CI

S02 GitHub run 41374594912 已 completed/failure，不再沿用旧的 in_progress。5 项失败是发现测试夹具的路径别名：检测器索引使用 canonical 物理路径，受控 backend 返回临时目录的别名。主管用项目内 junction TEMP 复现完全相同的 5 项失败（其余 6 项通过），将 discovery/tests.rs::temp_root 统一到物理路径后，11/11 相关测试通过，断言未删减，未放宽产品信任规则。

此小修尚未提交，和 S03 回交改动一同保留；它不修复上表历史缺陷。远端旧 run 仍为 failure，新 CI 只能在下一次已验收提交后另行验证。

## 证据与衔接

项目根 scratch/S03/acceptance：test-submitted.log（受限首次）、test-native.log、frontend-submitted.log、adversarial-submitted.log、adversarial-complete.log、production-budget.log、ci-alias-before.log、ci-alias-after.log、probe/、cases/、supervisor-state.json。原始证据不进入 app 仓库。

下一步仅手动转交项目根 dispatch/S03-R1-DeepSeek.md。仍以 S02 commit 为 base，保留当前未提交的整阶段实现与主管测试夹具小修；不 reset、不另建新阶段、不提交不完整实现。修复完成重新 SUBMITTED 后由 Sol 冻结并独立验收。

补充工具记录：一次未加 sol_s03:: 过滤的独立探针命令额外执行了正式模块测试，208通过/3失败；3项均因 scratch manifest 下不存在正式仓库 docs/contracts 路径（os error 3）。该失败日志保留为 final-probe-unfiltered.log；正式仓库中这3项已包含在199/199通过内，独立探针按原范围12/12通过，未改断言/冻结 schema。

暂存检查仅另清理 codex parser 与测试文件末尾的多余空白行；字节断言证明除 EOF 换行外全部内容与受测版本一致，记录在 final-whitespace-normalization.json。没有更改 Rust token/行为，未因此机械重复功能测试。
