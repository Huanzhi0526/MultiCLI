# MultiCLI 0.1.0

Windows 本地多 CLI 工作台。在一个窗口中浏览工作区、Agent 与会话，搜索历史，使用托管终端和结构化对话，并查看具有可靠来源的用量数据。

**当前版本：0.1.0，Windows 11 x64，用户级未签名安装包。** 项目已进入试用、功能迭代、体验优化、性能与运维维护阶段。

## 从哪里开始

| 你要做什么 | 阅读 |
|---|---|
| 安装和试用 | [快速上手](QUICKSTART.md) |
| 接管维护、修 Bug 或做优化 | [项目交接](HANDOFF.md) |
| Claude / Codex / 其他 Agent 开发 | [AGENTS.md](AGENTS.md)、[协作说明](CONTRIBUTING.md) |
| 编译和运行 | [开发环境](docs/DEVELOPMENT.md) |
| 备份、排错、升级和卸载 | [运维说明](docs/OPERATIONS.md) |
| 支持哪些 CLI | [兼容性](COMPATIBILITY.md) |
| 版本内容与限制 | [版本记录](CHANGELOG.md)、[已知限制](docs/KNOWN-LIMITATIONS.md) |

## 当前能力

- 工作区与 Agent 双向导航，访问同一份会话和消息。
- 只读索引与搜索 Codex、Claude、CodeBuddy 的已观察历史格式。
- 发现本机 CLI 安装与外部进程；外部进程只读观察。
- 多个自有 ConPTY / xterm 终端，中文输入、托盘后台运行与明确退出。
- 三核心结构化新建、权限处理、工具事件、取消和同会话继续。
- Codex 官方额度读取、Claude 本地转录 token；无可靠出口的产品明确显示未知或不支持。
- 自定义 CLI、当前 profile 的备份恢复和白名单诊断导出。

模型请求仍使用各 CLI 自己的服务与登录。MultiCLI 没有自己的远程控制、遥测、云同步或自动更新服务。不同产品和版本的实际覆盖见兼容性表。

## 仓库结构

```text
HANDOFF.md / STATUS.md       当前维护交接和状态
AGENTS.md / CLAUDE.md        Agent 入口
src/multicli/                当前产品 React 界面
src-tauri/src/multicli/      Rust 领域与运行时
src-tauri/src/multicli_bootstrap.rs  当前启动、IPC、托盘和退出
docs/                       架构、开发、运维、性能和技术契约
.github/workflows/          Windows 构建与检查
```

其他继承自上游的源码不代表已在 MultiCLI 中启用。产品启动边界见架构说明。

## 源码与安装包

正式源码：<https://github.com/Huanzhi0526/MultiCLI>。本项目的版本标签使用 `multicli-v0.1.0`，与继承的上游标签区分。
安装包可从已成功的 GitHub Actions 构建下载；包仍未签名。当前本机验证过的安装包及摘要见 [0.1.0 版本基线](docs/releases/0.1.0.md)。

提交源码、锁文件、文档和构建脚本即可协作。不要上传 node_modules、target、真实历史、数据库、终端日志、凭据或本机工具缓存。

## 来源与许可证

派生自 [Desktop CC GUI](https://github.com/zhukunpenglinyutong/desktop-cc-gui) 固定提交 `6c46345a727e7c1b57e0dacecb4fe063d88ee8dc`，保留上游 Git 历史与 MIT 版权。
参见 [LICENSE](LICENSE)、[第三方来源](THIRD_PARTY.md) 与 [继承范围](docs/UPSTREAM.md)。
