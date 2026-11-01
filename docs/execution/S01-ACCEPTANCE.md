# S01 阶段验收记录

2026-11-01，Sol。**数据层已 ACCEPTED；S01 整阶段仍 IN_PROGRESS**。IPC、双向导航、工作区规范化与诊断待下一次完整阶段回交，不能把数据层通过写成 S01 全部完成。

## 执行节奏与来源

用户本轮调整为整阶段派工，S01 剩余范围合并一次交接；原 S01-01/S01-R1 编号仅用于追踪已经发生的开发与修复，不再单独派 S01-02、S01-03。原 plan 保留，当前变更以本记录和 STATUS 为准。

主要实现由 DeepSeek 完成，用户确认配置为 WorkBuddy / deepseek flash 1017 / 1M / max。基线为 c3db27a8158bb0fef03a82dfafb74a7302250a58。用户明确授权验收剩余小问题由 Sol 直接修复或酌用 Luna；本轮由 Sol 直接修复，未新增 Agent。

## 独立验证与修复

| 验证 | 实际结果 |
|---|---|
| R1 回交的 locked check、正式测试、S00 回归 | PASS；34 项领域 + 3 项 S00 |
| 原六组缺陷的主管探针 | 9/9 PASS，均由 Sol 重新执行 |
| 相邻四项缺口修复前 | 扩展探针 9 PASS / 4 FAIL；不是仅靠静态判断 |
| Sol 小修后最终 locked check | PASS；未增加依赖或更改冻结 schema |
| 最终正式领域测试 | 38/38 PASS，0 ignored；34 项保留，Sol 新增 4 项回归 |
| 最终 S00 原生回归 | 3/3 PASS，含真实 Windows ConPTY |
| 最终主管独立探针 | 13/13 PASS，0 ignored；真实源码模块、合成数据库，无 mock |
| Windows 构建边界检查 | 4/4 PASS；统一 Test 脚本已同时运行领域与 S00 回归 |
| 当前源码原生构建 | cargo build --release --locked PASS，6m24s；保留 499 条旧模块告警 |
| 当前源码真实桌面 | test profile；baseline_info IPC 返回真实目录/基线；实际按钮的 inputEcho/unicodeCwd/resize/exitSuccess 均 true；关闭后 app/WebView 退出，自有 Vite 已停止 |

DeepSeek 修复了原首轮六组问题：跨会话/Agent/Host 引用归属、外部观察 owned 关联、安装身份冲突假成功、junction 指入旧数据根、较新库拒绝前写 WAL、required nullable 字段缺失未拒绝。原失败日志与数据库保留，没有修改旧探针来制造通过。

Sol 在本次明确授权下另补四个小缺口：安装 executable.hostId 必须匹配安装 host；Workspace 链接必须和 Session 同 Host；进程链接必须和 Run 所在 Session 同 Host；没有 schema_migrations 的未知非空库在 WAL/迁移之前拒绝，原字节不变。关联校验与写入在同一事务中，失败无局部落库；合法同 Host 及空库路径仍有正例。

统一 verify.ps1 原来只执行 multicli_bootstrap；现在先执行 multicli:: 领域测试再执行原 S00 测试，旧断言保留。主要业务逻辑仍由 DeepSeek 编写，Sol 补正与新增正式回归均已单独留证。

## 桌面与发布边界

本次使用当前源码 Cargo release 原生二进制 + 实际 Vite 前端进行桌面 IPC 回归，不是安装包验收。直接 Cargo 构建的 Tauri 入口仍读取开发 URL，第一次因前端服务未启动出现 ERR_CONNECTION_REFUSED；补齐单份 127.0.0.1:1427 Vite 后真实链路通过，原始现场保留。不能声称本批重新构建或安装了 NSIS 包；现有交付安装包仍是 S00 对应源码版本。

本次原生二进制 SHA256：644926D8630D3DAB9047F336B400AC08493D78DC2D89C6C0F26EEF1B5A20236C。编译及桌面验证期间，最终源摘要无变化；完整受测清单、规范化 Git 对象对照及原始日志放在项目 scratch，不为每个子步骤复制一份进仓库。

S00 GitHub Windows CI 已独立核实 success：[运行 41304638979](https://github.com/Huanzhi0526/MultiCLI/actions/runs/41304638979)，对应源码 6927a033075fbfc0a7b6c7e630711cb6309eed4a。本批的新 CI 是独立状态，不能沿用旧结果写成 PASS。

## 证据、限制与下一步

原始证据：scratch/S01-01/acceptance-r1/ 的 submitted-source.json、final-tested-source.json、sol-final-check.log、sol-final-domain-and-bootstrap.log、sol-platform-tests.log、release-build.log、native-ui-pass.json、runtime-result.json；独立反例在 scratch/S01-01/acceptance/sol-r1-independent.log、sol-r1-adjacent-before.log、sol-r1-adjacent-after.log。均不包含真实会话导入；合成失败现场不删除。

数据层的 3 Agent / 2 工作区 / 跨工作区 Session、分页与计数、迁移备份/失败恢复已有测试；UI A01、完整路径 A02、诊断 A20 仍待 S01 整阶段余项。真实 CLI 接入、历史索引、权限/额度、生产安装升级卸载、并发压力和性能仍 NOT_RUN。

开发回交中自述的额外审查 Agent 不作为主管独立证据；后续主开发继续由用户选定 DeepSeek 单角色完成，不自动加开发团队。原有错误记录作为历史保留；当前结论以本页为准。

本页随已验收数据层保存，具体 commit 与远端核验由 Git HEAD/origin/main 和根 STATUS.md 记录，避免反复为回填自身 SHA 创建文档提交。下一次只交一张 S01 完整剩余任务，开发未自动启动。
