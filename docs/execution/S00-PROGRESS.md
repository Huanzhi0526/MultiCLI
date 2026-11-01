> 历史实施记录：以下为早期逐步证据，可能含当时的等待/失败。最终状态以 S00-ACCEPTANCE.md 与 STATUS.md 为准，S00 已 ACCEPTED。

# S00 实施与独立证据记录（未验收）

2026-11-01，Sol。状态 IN_PROGRESS；S00 ACCEPTED / COMMITTED / PUSHED 均未发生。DeepSeek 未派工。

## 范围与结果

已保留原 plan，派生 app 仓库并保留上游 Git 历史；固定六个参考并按实际文件阅读。对上游原前端先构建，再增加独立启动、身份与数据/WebView 目录、严格本地 CSP、最小 IPC、真实 PTY 探针与构建入口。未写 S01–S07 业务实现。未启用远程、插件、遥测、电脑控制、自动更新或旧数据迁移；其运行态核查仍待桌面启动。

用户明确启用施工，并调整本项目为手动转交 DeepSeek 提示词与回交，不使用 Baton。没有第三个 Agent。原 AGENTS.md 已保留并追加本轮授权说明；未覆盖为模板。

| 验证 | 实际结果 | 证据（项目根 scratch/s00 下） |
|---|---|---|
| 原前端 pnpm build | PASS，原 CSS/chunk 警告保留 | logs/upstream-frontend-build.log |
| 隔离前端 verify -Scope Frontend | PASS | logs/multicli-frontend-build.log |
| Windows/发布边界 | 4 项 PASS，无跳过 | logs/platform-build-tests.log |
| Cargo Windows fetch --locked | PASS | logs/cargo-fetch.log |
| Rust check | FAIL_ENV：proc-macro build script 缺 link.exe；尚未检查应用源码 | logs/multicli-rust-check.log |
| 新 Rust 脚手架格式 | rustfmt 1.10.0 / toolchain 1.99.1 PASS | 当前格式化源码；不等于编译 |
| 领域/事件/错误 Schema | Draft 2020-12 语法 PASS | docs/contracts 三个 schema |
| Schema 边界反例 | origin=acp、缺 creationTime、executionStatus=awaiting-input 均拒绝；对应合成正例通过 | 主管工具执行记录；不等于 S01 数据层验收 |
| Codex 实际版本、schema、initialize | 0.166.0；321 schema；initialize/initialized PASS，无推理 | protocol-profiles/codex、codex-schema-0.166.0；protocol-probes.json |
| CodeBuddy ACP initialize | 2.179.0；协议 1 PASS，无推理 | protocol-profiles/codebuddy；protocol-probes.json |
| Claude help / TRAE 原生入口 | 2.1.153 / 0.209.1，version/help PASS；完整交互 NOT_RUN | logs/*help*.txt；COMPATIBILITY.md |
| 许可元数据导出 | 1089 条，无缺失 license 字段；未完成最终分发资产审核 | docs/research/dependency-licenses.json |
| Git diff 空白检查 | PASS | 主管 git diff --check |
| 真正桌面/IME/完整终端/安装卸载 | NOT_RUN | 等待原生检查/打包通过；不得用前端构建代替 |
| commit / push / CI | NOT_RUN | origin 已连接 Huanzhi0526/MultiCLI；未提交/推送 |

本机 Codex sessions、Claude projects、CodeBuddy sessions/projects 候选目录存在且可列目录；没有读取会话正文、解析源文件、导入或改写。TRAE 常见候选目录未发现，不据此推断它没有历史。历史格式与来源确认留待后续；原始目录探针只在 scratch/history-root-probes.json。

## 原生继续与验收门

微软签名安装器经 UAC 完成组件安装，17:18 exit 0；早期下载曾多次超时。随后确认工具与 SDK，并实际开始 Rust 编译。一次性本地脚本在源码冻结后等待本次安装就绪，最多 12 小时；只顺序运行 Rust/Test/Package，错误即停，保留日志。它不运行模型、不修改源码、不提交 Git，也不能替主管判定 ACCEPTED。

原生 build/test/package 即使全部通过，仍须普通用户桌面与实际 IPC/PTy 按钮、dev/test/prod 数据隔离、后台监听/旧数据无污染、用户级安装与二进制 SHA256 核查。S00 未通过不发 S01。用户已指定 GitHub 公开仓库 Huanzhi0526/MultiCLI 并完成 origin 关联；S00 独立验收通过后由主管审查、提交和推送。


## 17:38 环境修复 ENV-R1（未验收）

微软组件下载及安装完成：580 个唯一文件、1,608,638,046 B，安装器 17:18 exit 0，MSVC 14.44.35207、SDK 10.0.26100.0。17:23 的续验不是链接器缺失，而是 Git 自带 MSYS Perl 缺 Locale::Maketext::Simple，导致 OpenSSL Configure 失败；原日志已保留。确认控制脚本/编译停止及源摘要一致后修复 enter-env.ps1，选择并检测完整本机 Perl 的 IPC::Cmd 与 Locale::Maketext::Simple；纯前端检查不强制该原生前置。

采用官方 Strawberry Perl 5.42.3.1 x64 portable，在项目 scratch/tools/strawberry 解压，不修改全局 PATH、不引入其 GCC 路径、不改变 Cargo 依赖。官方校验来源 https://strawberryperl.com/releases.json；归档 SHA256 为 6a081a811781c30aca51dbc036afd93092af91e3297901f02c17043795a10690，304,765,269 B。下载校验、模块探针和重跑结果独立记录，归档存在本身不算修复成功。


ENV-R1 结果：归档长度与 SHA256 校验通过，所选项目内 perl.exe 报 v5.42.3，IPC::Cmd / Locale::Maketext::Simple 模块探针 exit 0。存在 C.UTF-8 不可用并回退本机中文 locale 的警告；不修改系统 locale。已确认旧续验与编译进程退出。接下来保存新源摘要并重跑单份 Rust→Test→Package；这些命令及桌面/安装验收仍未通过，不提交或推送。
