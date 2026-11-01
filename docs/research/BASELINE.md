# S00 底座准入

状态：**ACCEPTED（2026-11-01）**。采用 Desktop CC GUI v1.2.0 固定派生底座，commit 6c46345a727e7c1b57e0dacecb4fe063d88ee8dc；不启用同栈小骨架退路。app 保留原 Git 历史、MIT 版权和可复用模块。

| 准入项 | 独立结果 |
|---|---|
| 固定依赖恢复、原前端与隔离前端构建 | PASS；原 CSS/chunk 警告保留 |
| Rust locked check | PASS；S00-R1 修复协议特性/配置不一致 |
| 原生测试 | 3/3 PASS，0 ignored；上游 1288 项由过滤器排除，未声称全仓测试通过 |
| 普通用户桌面 | release 真正窗口启动；父进程 medium integrity |
| 真实 Windows PTY | dev/test/prod 与安装版 UI 按钮：回显、中文/空格 cwd、resize、退出全部 true |
| 数据/WebView 目录 | 三配置各自目录；WebView 进程命令行与 UI 路径一致 |
| Windows 用户级 NSIS | 构建 PASS；安装 exit 0，HKCU 登记，安装版启动与 PTY PASS |
| 三核心入口隔离 | 源码映射明确；Codex/CodeBuddy 无推理 initialize PASS；Claude 版本/help 已核对；完整业务交互 NOT_RUN |
| 默认后台边界 | 仅两个 S00 IPC；无旧启动/插件；运行时所检进程树 TCP 连接为空，关闭后应用/WebView 退出 |

当前 run 只调用 multicli_bootstrap；上游 startup 与 preview 注册保留源码但排除编译（cfg(any())）。Cargo 关闭 protocol-asset，与 assetProtocol=false 一致；不得直接恢复上游入口或全部旧 IPC。

identifier 为 local.multicli.workbench。数据为 %LOCALAPPDATA%/MultiCLI/{dev,test,prod}，WebView 在各配置 webview 子目录；debug 默认 dev、release 默认 prod。原生测试验证默认值，三个 release 显式配置均经真机验证；不继承上游 portable-data.json。

限制、原始证据位置、安装包 SHA256 及尚未执行的最终发布验收见 docs/execution/S00-ACCEPTANCE.md。准入不表示保留的上游全部能力已开放。
