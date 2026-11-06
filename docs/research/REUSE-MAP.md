# 来源追踪

| 目标 | 来源与用途 |
|---|---|
| 原继承源码、资源和模块 | Desktop CC GUI固定提交6c46345a727e7c1b57e0dacecb4fe063d88ee8dc；保留MIT LICENSE、原版权和Git历史 |
| src/multicli、Rust multicli、multicli_bootstrap、产品契约与维护文档 | 本项目新增或在既有集成边界独立实现，可通过相对固定上游的Git diff追溯 |
| 当前独立图标 | 本项目public/multicli.svg及src-tauri/multicli-icons，不沿用上游应用标识 |
| 首页、CLI图标选择与柔和侧栏的视觉参考 | AionUi 固定提交11cd2a9a20d1e033157636efc570b41384c66d85（Apache-2.0），只读参考 resources/homepage.png、packages/desktop/src/renderer/pages/guid/index.module.css、components/chat/SendBox/sendbox.css、components/layout/Sider/SiderNav/SiderToolbar.tsx、styles/layout.css；MultiCLI 使用自身 React/API 独立实现，没有复制其代码、品牌或助手功能 |
| CLI选择栏产品标识 | 沿用继承源码内 src/assets/model-icons/claude.svg、codebuddy.svg 与既有依赖 @lobehub/icons-static-svg/icons/openai.svg；保留原资源与依赖许可，未用这些标识替换 MultiCLI 应用品牌 |
| 其他参考仓库 | references.lock.json中的固定只读参考；当前没有据此声明额外代码移植 |

后续复制第三方代码时记录来源URL、固定commit、原路径、用途和许可证；不要把“阅读参考”说成已经集成。
依赖许可元数据见dependency-licenses.json，它不代替完整分发许可文本或NOTICE审查。
上游版本说明的原始内容保留在Git历史及用户本地归档；本产品内置版本记录现为MultiCLI0.1.0。
