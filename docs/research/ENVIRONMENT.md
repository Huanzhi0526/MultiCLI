# Windows S00 环境（2026-11-01）

## 实测快照

| 项目 | 实际结果 |
|---|---|
| OS | Windows 11 企业版 x64；10.0.26300 |
| CPU / RAM | Ryzen 7 7735H，8 核 16 线程；16,366,567,424 B 物理内存 |
| 开始时磁盘可用 | C 约 85.9 GB，D 约 460.7 GB（不是当前余额） |
| Git | 2.56.0.windows.2，作者 name/email 已配置 |
| Node / npm | v24.16.0 / 11.13.0 |
| pnpm | 全局 11.7.0 未改；本项目 Corepack 使用 packageManager 固定 10.29.3 |
| PowerShell / Python | 7.6.5 / 3.10.2 |
| Rust / Cargo | 1.99.1；rustfmt 1.10.0 已装于固定 1.99.1 工具链；官方 rustup 安装器 SHA256 校验；项目 scratch/tools 内安装，未修改系统 PATH |
| MSVC / Windows SDK | 17:18 安装器 exit 0；MSVC 工具目录 14.44.35207，Windows SDK 10.0.26100.0 |
| WebView2 | 本机已发现 151.0.4129.107 |
| GitHub connector | 用户身份查询成功；不等于本机 Git transport 已认证。目标 Huanzhi0526/MultiCLI 为公开空仓库 |
| Git 远端 | origin 为 https://github.com/Huanzhi0526/MultiCLI.git，默认推送 origin；upstream 仅作来源；尚未提交/推送 |

## 构建入口

在 app 中用 `./scripts/multicli/verify.ps1 -Scope Frontend|Rust|Test|Package|All`。enter-env.ps1 只修改本进程环境，自动使用项目工具缓存；CI 可使用预装工具。Rust 并发限制为 2，以本机 16 GB 内存为起点，未进行性能验收。

前端依赖 `--frozen-lockfile --ignore-scripts` 成功；真实上游前端与隔离前端构建通过。Cargo Windows 目标 `fetch --locked` 成功。第一次 Rust check 失败在 proc-macro2/quote 的 build-script 链接：`link.exe not found`。安装 MSVC 后的第二次检查推进到 openssl-sys，但因 Git Perl 缺模块停止；完整应用源码检查仍未完成。不得写成 Rust/桌面 PASS。

MSVC 自动下载曾超时；复用本机签名有效的 Microsoft Visual Studio Installer 4.6.58，第一次非管理员返回 5007。随后经 Windows UAC 以管理员运行，目标只包含 VCTools + x86/x64 compiler + Windows11 SDK 26100；`--noUpdateInstaller --norestart --nocache`。安装器早期获取 VisualStudio.vsman 曾超时重试，后已继续完成。官方组件依据：[Microsoft 组件表](https://learn.microsoft.com/en-us/visualstudio/install/workload-component-id-vs-build-tools?view=vs-2022)；[命令参数](https://learn.microsoft.com/en-us/visualstudio/install/use-command-line-parameters-to-install-visual-studio?view=vs-2022)。

组件清单后续取得并完成实际组件安装，详见 dd_setup_20261101155625.log；17:18 安装器 exit 0，随后确认 MSVC/SDK。未更改 Hyper-V、WinNAT、系统代理、系统安全策略或现有 CLI 全局配置。网络诊断的 --noproxy 仅作用于单条 curl 请求。启动 DSH Web 的诊断失败后停止采用：用户明确本项目不用 Baton；未向模型派工，未改凭据或默认模型。

## 可复现现场（相对项目根）

- scratch/s00/environment-host.json：硬件/初始工具环境。
- scratch/s00/logs/upstream-frontend-build.log、multicli-frontend-build.log：前端证据。
- scratch/s00/logs/cargo-fetch.log、multicli-rust-check.log：Rust 依赖与环境失败。
- scratch/s00/install-msvc-existing.ps1、msvc-install.json：安装脚本和最近已结束尝试结果；运行中安装器需结合 dd_installer 日志，不以旧 5007 判断当前尝试。
- `%LOCALAPPDATA%/Temp/dd_installer_20261101154223.log`：本次管理员安装器日志，真实现况以该日志和 vswhere 为准。
- scratch/s00/protocol-probes.json、protocol-profiles/：不含推理的隔离初始化与 Codex 导出 schema。

工具安装目录不能作为普通临时日志随意清理；MSVC 若成功将由 Windows Installer 登记路径，卸载应走其正式卸载流程。原始日志、下载文件、CLI profile 不进入源码仓库。


## 17:38 环境修复 ENV-R1（未验收）

微软组件下载及安装完成：580 个唯一文件、1,608,638,046 B，安装器 17:18 exit 0，MSVC 14.44.35207、SDK 10.0.26100.0。17:23 的续验不是链接器缺失，而是 Git 自带 MSYS Perl 缺 Locale::Maketext::Simple，导致 OpenSSL Configure 失败；原日志已保留。确认控制脚本/编译停止及源摘要一致后修复 enter-env.ps1，选择并检测完整本机 Perl 的 IPC::Cmd 与 Locale::Maketext::Simple；纯前端检查不强制该原生前置。

采用官方 Strawberry Perl 5.42.3.1 x64 portable，在项目 scratch/tools/strawberry 解压，不修改全局 PATH、不引入其 GCC 路径、不改变 Cargo 依赖。官方校验来源 https://strawberryperl.com/releases.json；归档 SHA256 为 6a081a811781c30aca51dbc036afd93092af91e3297901f02c17043795a10690，304,765,269 B。下载校验、模块探针和重跑结果独立记录，归档存在本身不算修复成功。


ENV-R1 结果：归档长度与 SHA256 校验通过，所选项目内 perl.exe 报 v5.42.3，IPC::Cmd / Locale::Maketext::Simple 模块探针 exit 0。存在 C.UTF-8 不可用并回退本机中文 locale 的警告；不修改系统 locale。已确认旧续验与编译进程退出。接下来保存新源摘要并重跑单份 Rust→Test→Package；这些命令及桌面/安装验收仍未通过，不提交或推送。


## 最终验证回填

18:28 Rust locked check、3 项原生测试和 NSIS 构建通过；普通用户桌面、dev/test/prod 数据/WebView 隔离、安装版启动及 PTY 通过。最终独立结果见 ../execution/S00-ACCEPTANCE.md。CI 尚未运行，原始安装/构建日志留在 scratch。
