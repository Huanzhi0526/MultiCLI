# MultiCLI

纯本地 Windows 多 CLI 工作台，当前处于 **S00 基线已验收，产品功能尚未实施**。

正式方案：[执行入口](docs/execution/plan/README.md)。当前状态：[STATUS](docs/execution/STATUS.md)。底座与工具链：[BASELINE](docs/research/BASELINE.md)、[ENVIRONMENT](docs/research/ENVIRONMENT.md)。能力实测：[COMPATIBILITY](COMPATIBILITY.md)。接口：[contracts](docs/contracts/README.md)。上游原说明保留在 [UPSTREAM-README](docs/research/UPSTREAM-README.md)。

源自 Desktop CC GUI 固定 v1.2.0 / `6c46345a727e7c1b57e0dacecb4fe063d88ee8dc`，保留 Git 历史与 MIT 许可。新启动接缝不会运行旧迁移、CLI skill 安装、DSH 自动启动、远程服务、遥测、插件或更新；这些旧源码不是当前产品能力。

## 本地构建

Windows x64；Node 24.16.0；项目 pnpm 10.29.3；Rust 1.99.1 MSVC；Microsoft C++ Build Tools + Windows SDK；WebView2；完整原生 Perl（含 IPC::Cmd / Locale::Maketext::Simple，实测 Strawberry 5.42.3.1）。

```powershell
corepack pnpm install --frozen-lockfile --ignore-scripts
./scripts/multicli/verify.ps1 -Scope Frontend
./scripts/multicli/verify.ps1 -Scope Rust
./scripts/multicli/verify.ps1 -Scope Test
./scripts/multicli/verify.ps1 -Scope Package
```

依赖与检查以锁文件和脚本为准；本机 S00 已构建并安装测试包，SHA256 和实测范围见 docs/execution/S00-ACCEPTANCE.md；后续源码变化需重新验证。当前临时窗口只显示真实数据目录，并手动运行自有 cmd.exe PTY 探针；产品导航与功能由后续 DeepSeek 小批次实施。

应用标识 `local.multicli.workbench`，应用数据和 WebView 数据均归 `%LOCALAPPDATA%/MultiCLI/{dev,test,prod}`。debug 默认 dev，release 默认 prod；可用 `MULTICLI_PROFILE=test` 启动隔离试验，不继承 CC GUI 的 portable-data.json。产品关闭到托盘语义待 S04 实现，S00 窗口仅为基线探针。

## 施工方式

GPT Sol 主管、DeepSeek 开发，仅两个角色。本项目不用 Baton。Sol 准备单批提示词，由用户手动转交；DeepSeek 回交未提交代码并停止写入，Sol 独立验收后 commit/push。原始历史、凭据、数据库、下载参考和原始日志不入 Git。
