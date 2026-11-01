# S00 独立验收

2026-11-01，Sol。结果 **ACCEPTED：环境与底座准入范围**。开发 Agent 未派工；以下不是成品验收或最终发布验收。

## 受测来源

Desktop CC GUI v1.2.0 / 6c46345a727e7c1b57e0dacecb4fe063d88ee8dc，分支 multicli/s00。构建前冻结完整源文件摘要，构建结束及桌面验证后均核对一致。当前正式报告属于构建后的文档回填；产品源码和配置未改变。受测代码清单见 S00-tested-source.json；原完整摘要与原始日志在项目根 scratch/s00。

## 实际验证

| 检查 | 结果及证据 |
|---|---|
| 前端 | 原版及隔离版类型/构建 PASS；构建边界 4/4 PASS，无跳过 |
| Cargo | Windows locked fetch PASS；最终 locked check PASS；保留旧模块造成大量 dead_code 等警告，未执行自动清理 |
| 原生测试 | 3/3 PASS、0 ignored；真实 cmd.exe ConPTY、配置隔离及拒绝非法 profile。1288 个上游测试被过滤，不属于本次覆盖 |
| 安装包 | 18:28 完成 NSIS 构建，4.84 MiB（5,079,176 B）；未签名测试包 |
| 普通权限桌面 | medium-integrity 父进程启动 release，多次观察真实窗口；没有管理员应用进程要求 |
| dev/test/prod | 分别真实启动；UI 与 WebView --user-data-dir 均指向各自目录。三者都点真实 PTY 按钮，inputEcho/unicodeCwd/resize/exitSuccess 均 true |
| 安装版本 | /S 用户级安装 exit 0；HKCU 登记 MultiCLI 0.1.0；无 profile 环境变量启动默认 prod，真实 PTY 四项均 true |
| 退出 | 关闭窗口后应用及 MultiCLI WebView 子进程退出；本阶段尚无托盘能力 |
| 后台边界 | 源码仅注册两个 S00 IPC，不运行上游启动。检查 prod 进程树时 TCP 连接/监听为空；这不是全程网络抓包或 WebView 系统组件的遥测审计 |
| 数据保护 | .ccgui-next 在本机不存在，整个测试后仍不存在；Codex/Claude/CodeBuddy 明确配置文件摘要/元信息不变。未导入真实历史；不声称完成历史索引只读验收 A06 |
| 发布范围 | 全部新增/修改文件列表人工审阅，补充秘密模式扫描无命中；无 scratch/参考仓库/数据库/凭据/原始历史/安装包进入 Git。保留 MIT 和 agency-agents LICENSE，第三方依赖许可元数据已记录 |

## 安装包与二进制

包：MultiCLI_0.1.0_x64-setup.exe。

SHA256：3C35A14F050C78D65CD7109B3E8CAEE218EFE7479A13557220A5BFE2A26DE054。

release exe SHA256：4869C14C0C34EE211256EC50BDE40BCF5C3ADB743D62199463B2E2F27D87675E。

安装 exe SHA256：99EB3189F83B71C94584F74B1BE02A18EE10138B77635A7B1EBC49762A83FCAC。两者长度均 6,543,360 B；独立逐字节比较只有 3 字节不同，Tauri bundle type 标记由 UNK 改为 NSS，与打包日志相符。

由 Codex Windows 宿主启动的安装测试中，桌面工具报告了 MSIX LocalCache 下的物理路径，而应用、CIM 与 HKCU 展示标准 LOCALAPPDATA 路径。此宿主映射已保留证据；不能据此宣称干净账户、非打包宿主及最终升级/卸载验收完成，S07 必须补测。

## 未覆盖与后续门

- S01–S07 产品功能全部 NOT_RUN。三核心真实新会话、权限拒绝/允许、工具事件、继续/取消及认证状态未验；帮助/握手不能替代 S05。
- S04 完整交互终端、IME/粘贴/方向键/Ctrl+C、8 终端负载、托盘与自有树清理仍 NOT_RUN。此处只验 S00 最小 PTY。
- S07 干净用户安装、升级、卸载及数据保留/删除、签名、完整分发许可文本/NOTICE/资产审查和性能验收仍 NOT_RUN。
- CI 尚未运行；本地通过不代表 GitHub Actions 通过。公开 Release 未授权，也未发布。
- 原始日志：logs/deferred-rust.log、deferred-test.log、deferred-package.log；UI 原始文本：prod/dev/test/installed-desktop-ui.json；进程/网络/数据保护：相应 *-processes.json、prod-desktop-network.json、protected-data-before/after.json，仅在 scratch/s00。

S00 允许进入 S01-01；先保存本批源码并核实远端，再给用户完整手动派工卡。
