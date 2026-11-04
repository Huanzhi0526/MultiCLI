# MultiCLI Agent 协作约定

当前产品版本 0.1.0，工作重点是用户试用反馈、功能更新、体验优化、性能和运维。先读 HANDOFF.md 与 STATUS.md，再按任务阅读 docs/README.md 中的相关文档。

## 工作上下文

- 实时检查分支、工作区 diff 和相关实现，保护已有用户改动。文档描述的是基线，不代替当前代码和 Git 状态。
- 不固定主管、开发者或模型身份，不沿用初期阶段任务卡与返工轮次。没有用户授权，不自动委派其他 Agent 或启动后台施工流程。
- 不同设备通过仓库中的文档、分支与 PR 同步；本地聊天、账号和 scratch 不作为远程协作前提。
- 功能开发与优化围绕用户本次目标推进；路线图是候选方向，不是自动开工清单。

## 产品边界

- 当前入口是 src/main.tsx → src/multicli/App.tsx；原生入口为 multicli_bootstrap。不要误接回上游 bootstrap、旧数据库或插件/relay/遥测入口。
- 应用自有数据在 %LOCALAPPDATA%/MultiCLI/{dev,test,prod}；不采用上游 portable-data.json 或 .ccgui-next 数据目录。
- 外部进程只读观察，不接管、不注入、不自动终止；历史源只读。只管理本项目明确创建的子进程。
- Session、Run、Process 身份分离；继续会话创建新 Run。未知能力、数值和错误保持真实，不伪造支持、余额或成功。
- 备份与恢复限当前 profile；数据库/WAL/SHM、迁移和恢复失败路径必须保留可回退性。

## 修改与验证

- 修改 IPC/DTO 时同时核对前后端和 docs/contracts；UI 文案使用 src/i18n/multicli.ts，界面行为同步维护 docs/ui-ux-spec.zh-CN.md。
- 诊断保持白名单，避免输出或上传消息正文、凭据、完整命令和本机隐私路径；终端日志与备份按敏感数据处理。
- 以根因和可观察行为为依据修改；保持正文、顺序、Unicode/emoji 和生命周期完整。不要靠吞错误、放宽断言或无界缓存掩盖问题。
- 按影响范围运行检查，说明实际覆盖与未测范围。文档整理无需重复原生全量构建；Windows 终端、托盘、进程和安装结论需要 Windows 环境。
- 开发和验证入口见 docs/DEVELOPMENT.md。版本发布时同步 package.json、Tauri、Cargo 的产品版本；版本说明同步 CHANGELOG.md、docs/releases/ 和 src/version/changelog.ts。
- 依赖、许可证和上游来源信息应保留。不要为整理文档删除未分析的上游源码或打包资源。

## Git 与交付

- 用户对当前任务明确授权提交/推送后再执行；授权不自动扩展到其他任务、合并他人 PR、部署或公开 Release。
- 多设备使用独立分支或 worktree，避免两个 Agent 同时修改同一 checkout。PR 写清变化、验证和实际限制。
- 不 force push、不提交密钥、真实历史、数据库、工具链、node_modules 或 target。
- 完成后更新 STATUS.md；架构、启动方法或运维边界改变时更新相关文档。交接重点是当前结果、未解决问题和下一步，避免叠加过期快照。
