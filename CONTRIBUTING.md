# 开发与跨设备协作

本仓库欢迎用户选定的Claude、Codex和其他开发者参与。共享上下文是README、HANDOFF、STATUS和相关技术文档，不是某个聊天的完整历史。

## 开始一次修改

先读取AGENTS.md、HANDOFF.md、STATUS.md并检查Git状态，然后围绕用户本次目标定位实现。不要根据旧阶段编号或模型名称猜职责。
多个设备使用各自分支；同一机器需要并行时使用独立worktree，避免共同写同一checkout。

```sh
git clone https://github.com/Huanzhi0526/MultiCLI.git
cd MultiCLI
git switch -c feature/short-description
```

## 哪些文件需要同步

提交源码、锁文件、文档、合成测试和构建配置。不要提交node_modules、target、dist、全套工具链、CLI账号、原始历史、DB、终端日志或真实备份。
AGENTS.md是通用约定，CLAUDE.md是Claude入口，HANDOFF.md是维护交接，STATUS.md只写当前状态。改变长期行为时更新对应架构/运维/兼容性文档。

Claude的项目上下文机制参见[官方CLAUDE.md说明](https://support.claude.com/en/articles/14553240-give-claude-context-claude-md-and-better-prompts)。这些文件随源码克隆；各设备上的私有聊天和本机自动记忆不会因此自动同步。

## 云端与Windows的分工

Linux环境可以做文档、TypeScript/React修改及前端构建/相关测试；Windows原生进程、ConPTY、托盘、安装和文件路径行为需要Windows CI或本机证据。
构建不需要上传用户CLI登录信息。依赖按锁文件恢复，工具链按docs/DEVELOPMENT.md准备。真实模型交互使用开发者自己的授权账户，凭据不进入CI。

## 提交与PR

用户授权提交/推送后，只提交本次相关文件。PR说明具体行为变化、为什么修改、实际验证及仍存在的限制；未跑内容如实写明。
不要把构建通过等同于真实CLI或安装验证。提交PR不自动授权合并、改权限、部署或发布Release。
跨设备交接时在PR和STATUS写明当前结果、剩余问题、下一步及必要命令；本地私有证据只给摘要，不上传正文或账号数据。

## 版本更新

当前产品版本0.1.0。功能更新使用新版本号，文档修正可继续维护当前版本。涉及发版时同步产品版本文件、CHANGELOG与相关能力说明，使用multicli-v<版本>标签，保留已发布标签。
安装器和GitHub Actions产物不放入Git源码历史；公开Release需要用户明确授权。
