# S02 安装签名与识别规则

2026-11-02。本文件记录 S02 使用的产品签名、来源、正反样本与置信规则。签名是**数据**
（`src-tauri/src/multicli/discovery/signature.rs`，`SIGNATURE_SCHEMA_VERSION = 1`），
不是散落的正则；新增产品必须同时补正反样本，并让 `check_samples` 通过。

## 1. 范围

只声明三个核心产品：Codex CLI、Claude Code、CodeBuddy Code。其余六个产品族属 S06；
本阶段**不**因为多写几个品牌字符串就声称支持九族。`SignatureRegistry::standard()`
因此固定返回 3 条签名。

## 2. 真实安装形态（本机只读核对）

| 产品 | 实际入口 | 形态 | 版本输出 |
|---|---|---|---|
| Codex CLI | `%LOCALAPPDATA%\Programs\OpenAI\Codex\bin\codex.exe` | 原生 exe（同目录还有 `codex-exec-server.exe`、`codex-code-mode-host.exe`，**不**算 CLI 入口） | `codex-cli 0.166.0` |
| Claude Code | `%USERPROFILE%\.local\bin\claude.exe` | 原生 exe | `2.1.153 (Claude Code)` |
| CodeBuddy Code | `%APPDATA%\npm\codebuddy.cmd` / `.ps1` → `node_modules\@tencent-ai\codebuddy-code\bin\codebuddy`（Node，shebang `#!/usr/bin/env node`） | cmd/PowerShell 包装器 + Node 包入口 | `2.179.0` |

`%APPDATA%\npm\node_modules\@openai` 在本机是一个**空目录**（曾经的 npm 安装残留），
正好验证“目录存在不等于已安装”：它只会产生 `historical-residue`，不会产生
`installed`。

## 3. 来源与移植许可

- 三个 CLI 的安装布局、`--help` 与 `--version` 输出，均由本机只读执行/读取得到，
  没有复制任何上游源码。
- Node 包入口后缀来自各包公开的 npm 目录布局（`@openai/codex`、
  `@anthropic-ai/claude-code`、`@tencent-ai/codebuddy-code`）。
- 包装器规则来自 npm 生成的 `codebuddy.cmd` / `codebuddy.ps1` 文本本身（只读解析，
  不执行）。
- 未从 `references/` 的固定仓库移植代码；如果后续需要，必须在该文件登记来源与许可。

## 4. 签名要点

| 字段 | 作用 |
|---|---|
| `native_names` | 原生 exe 文件名（小写、含 `.exe`） |
| `wrapper_names` | shim 基名（无扩展名，小写） |
| `node_entry_suffixes` | Node 包入口后缀（小写、`\` 分隔，必须落在声明边界上） |
| `install_roots` | 可信目录：`PATH` + 产品自有安装根 |
| `residue_paths` | 只用于判定 `historical-residue` 的目录 |
| `known_flags` | 允许出现在脱敏命令里的结构性 flag |
| `cwd_flags` | 唯一可用于工作区关联的显式目录参数（只有 Codex 有 `-C/--cd`） |
| `version_markers` | 版本输出里必须出现的产品标记（CodeBuddy 无标记，用通用语义版本解析） |
| `positive_samples` / `negative_samples` | 签名自检矩阵 |

## 5. 识别规则（按优先级）

1. **已验证安装入口**：进程镜像的规范化路径等于某个 `installed` 条目的
   canonical 入口 → `confirmed`，证据 `installation-exe`。
2. **Node 包入口**：解释器是 `node`，`argv[1]`（跳过 flag）是声明的包入口，
   且该入口位于可信目录 → `confirmed`，证据 `node-package-entry`。
   `node -e ...` / `node --print ...` 直接判否，避免把内联表达式当产品。
3. **可信目录 + 同名 exe** → `likely`，证据 `path-entry+filename`。
   目录既不在 `PATH`，也不在声明的安装根下时**不**匹配（同名无关 exe 负例）。
4. 其余一律不匹配。

明确不做的事：

- 不扫描 `argv[2..]` 里的品牌词（提示词、`echo` 文本、脚本路径都不算）；
- 不把 `cmd.exe` / `powershell.exe` 本身当产品；包装器只作为“来源程序”出现在子进程
  的父链里，因此一个 cmd/ps1 包装器与其 node 运行时不会产生两份实例；
- 不把 `python`、`bash`、`conhost` 等通用运行时当产品。

## 6. 正反样本矩阵

`signature.rs` 内声明并在单元测试 `declared_positive_and_negative_samples_all_match`
中全部执行（共 17 条）。代表项：

正向：

- `codex.exe --version`（可信安装根）
- `codex.exe exec refactor the claude wrapper`（后面的品牌词不影响）
- `node ...\node_modules\@openai\codex\bin\codex.js --version`
- `claude.exe --version`（`%USERPROFILE%\.local\bin`）
- `node ...\@anthropic-ai\claude-code\cli.js --version`
- `node ...\@tencent-ai\codebuddy-code\bin\codebuddy -p hello`

反向：

- `node -e "console.log('launch codex or claude or codebuddy')"`
- `cmd.exe /c echo codex claude codebuddy`
- `python C:\Users\me\scripts\codex_helper.py`
- `C:\Users\me\Downloads\codex.exe --version`（同名但不可信目录）
- `...\Codex\bin\codex-code-mode-host.exe`（同目录的辅助可执行文件）
- `node ...\@anthropic-ai\claude-code-tools\cli.js`（近似但不是声明入口）
- `cmd.exe /c codebuddy --version`（只识别真正的 node 运行时）
- `node ...\codebuddy-code\README.md`（不是 Node 入口）

## 7. 包装器解析（受限数据）

`wrapper.rs` 把 `.cmd/.bat/.ps1` 当文本读，上限 64 KiB：

- 只展开 shim 自身目录变量（`%~dp0`、`%dp0%`、`$basedir`、`$PSScriptRoot`），
  其他 `%VAR%` / `$env:X` 原样保留（不读环境）；
- 找到最后一个 `node <entry>` 语句（npm shim 的实际执行行）；
- 入口必须位于 shim 目录内（`EscapesShimDirectory`），必须存在
  （`MissingTarget`），必须是 `.js/.mjs/.cjs/...` 或带 node shebang
  （`NotANodeEntry`）；
- 从不执行 shim，也从不起 `cmd.exe` / PowerShell。探测时直接以 argv 数组启动
  `node.exe <entry> --version`。

## 8. 覆盖边界

- 只做 Windows 本地。POSIX 无扩展名 shim 不解析（Windows 的 `PATH` 查找也不会用它）。
- `%APPDATA%\npm` 之外的其它全局前缀（pnpm、yarn、volta、fnm 等）不在 S02 的声明
  安装根内；找不到就如实不报，不猜。
- 版本探测有 8 秒上限与 5 分钟缓存；失败保留 reason，不降级成“未安装”。
- 短于扫描间隔的进程可能漏见；UI 与契约都写明“不是审计级全量记录”。
