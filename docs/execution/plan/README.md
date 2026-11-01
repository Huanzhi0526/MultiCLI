# MultiCLI 开工方案包

编制日期：2026-11-01。状态：**方案已准备，施工未启动，所有阶段 NOT_RUN**。

目标：纯本地 Windows 多 CLI 工作台，管理自己启动的 CLI，同时发现其他程序启动的 CLI；统一查看工作区、Agent、历史、进程和用量。主管为 GPT Sol，开发为 DeepSeek。DeepSeek 不提交 commit，Sol 负责环境、参考仓库、独立验收及 GitHub 提交。

本轮交付仅为方案，没有下载完整参考仓库、安装依赖、关联远端或启动开发 Agent。当前工作目录检查为空目录；本方案包是新增材料。原需求来源：[多CLI统一工作台对话](chatgpt-conversation://6ae6c23b-2608-83e9-a094-2f20efde1323)，已读取其四轮完整消息。

## 阅读入口

1. [产品与技术方案](01-方案.md)：最终范围、架构、模型、版本路线和风险。
2. [开源参考与查证记录](02-参考项目.md)：来源、取舍、下载范围、已核对事实。
3. [Sol 主管启动提示词](prompts/SOL-主管.md)：可直接复制给主管。
4. [DeepSeek 开发提示词](prompts/DEEPSEEK-开发.md)：每次配合当前任务卡发送。
5. [验收与 Git 工作流](03-验收与提交.md)：验收和提交唯一规范。

## 阶段顺序

| 阶段 | 执行者 | 内容 | 前置 |
|---|---|---|---|
| [S00](tasks/S00-环境与基线.md) | Sol | 下载参考、基线验证、隔离数据目录、GitHub 关联、冻结接口 | 用户启用施工 |
| [S01](tasks/S01-领域与导航.md) | DeepSeek → Sol | 核心数据模型、迁移、双向导航、能力声明 | S00 ACCEPTED |
| [S02](tasks/S02-发现与进程.md) | DeepSeek → Sol | 安装和外部进程发现、父子关系、误识别控制 | S01 ACCEPTED |
| [S03](tasks/S03-历史与搜索.md) | DeepSeek → Sol | 历史只读索引、全文检索、增量与恢复定位 | S02 ACCEPTED |
| [S04](tasks/S04-终端与生命周期.md) | DeepSeek → Sol | ConPTY 交互、多终端、进程生命周期 | S03 ACCEPTED |
| [S05](tasks/S05-三大核心接入.md) | DeepSeek → Sol | Codex、Claude Code、CodeBuddy 结构化接入 | S04 ACCEPTED |
| [S06](tasks/S06-额度与扩展识别.md) | DeepSeek → Sol | 用量中心、其余主流 CLI 识别和 PTY 路线 | S05 ACCEPTED |
| [S07](tasks/S07-打包与总验收.md) | DeepSeek → Sol | 集成、性能、安装包、用户说明与 GitHub 收尾 | S06 ACCEPTED |

阶段内部按任务卡继续拆成小批次，始终只有一个 DeepSeek 写代码。不得一次把 S01–S07 全交给开发模型。

## 开工使用方法

把 `SOL-主管.md` 与本目录路径交给 Sol，明确“开始按此方案施工”。Sol 完成 S00 后，每次发送 `DEEPSEEK-开发.md` 加一张具体阶段卡及本次子任务范围。DeepSeek 回交未提交改动，Sol 验收通过后提交、推送，再发下一批。

GitHub owner/repository/可见性尚未指定：S00 先检查现有远端和已登录身份；若无可用目标，在本地准备完成后只询问这组缺失信息，不猜测账号或创建公开仓库。这不妨碍现在阅读或采用本方案。

`templates/` 提供回交、验收及状态模板。规划文件中的命令和类型是施工约定，必须在 S00 对照锁定基线变成实际脚本与契约；不能把文档、mock 或上游 README 当成已完成的软件。
