# MultiCLI 0.1.0 兼容性

记录日期2026-11-04，表示明确版本和有限样本。识别到安装、启动帮助、真实模型回复与额度是不同能力。

| 产品 | 已观察版本 | 发现/PTY | 只读历史 | 结构化交互 | 用量 |
|---|---|---|---|---|---|
| Codex CLI | 0.166.0 | 真实发现及TUI启动 | 普通/压缩历史副本、索引与搜索 | 新建、正文、权限拒绝/允许、工具、取消、退出、同Session继续 | 官方机器额度与token有限样本 |
| Claude Code | 2.1.153 | 真实发现及信任目录界面 | 已观察JSONL格式、索引与搜索 | 同上，stream-json真实链路 | 本地转录token，不是余额 |
| CodeBuddy Code | 2.179.3 | 真实发现及cmd/ConPTY信任界面 | 已观察格式、索引与搜索 | ACP v1真实链路、同Session继续 | 无可靠机器出口，unsupported |
| TRAE CLI | 0.209.1 | 原生入口真实发现、PTY version/help | 未测 | 未测 | unsupported |
| Gemini CLI | 0.66.0 | 仅隔离安装fixture，真实发现/PTY version/help | 未测 | 未登录/未推理 | unsupported |
| OpenCode | 未确认真实安装 | 签名正负样本；真机未测 | 未测 | 未测 | unsupported |
| Qwen Code | 未确认真实安装 | 同上 | 未测 | 未测 | unsupported |
| GitHub Copilot CLI | 只有IDE shim候选 | 不视为独立CLI支持 | 未测 | 未测 | unsupported |
| Kimi Code CLI | 未确认真实安装 | 签名正负样本；真机未测 | 未测 | 未测 | unsupported |
| Aider | 未确认真实安装 | 同上 | 未测 | 未测 | unsupported |

三核心结构化链路通过生产RuntimeManager协议探针，各一条有效样本；不代表逐款所有权限卡都由人工点击过。
历史读取采用真实源文件只读副本和格式样本，不把其他版本或其他产品格式推断为兼容。
PTY基础中文IME、emoji、多行、resize、方向键、Ctrl+C和托盘由用户真机试验；这些不等于每款CLI的模型或认证能力。
九个计划产品族外额外识别TRAE；“产品族数量”不能替代逐能力证据。

此前CodeBuddy普通CLI可用但ACP认证失败的差异后来恢复，其原因没有证实；不能因此自动logout、重置登录或读取token。
后续CLI升级后重新核对相关入口和行为，保留未知状态。详细限制见[KNOWN-LIMITATIONS](docs/KNOWN-LIMITATIONS.md)。
