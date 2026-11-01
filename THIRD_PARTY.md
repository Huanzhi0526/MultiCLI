# 第三方来源

项目派生自 [Desktop CC GUI](https://github.com/zhukunpenglinyutong/desktop-cc-gui) v1.2.0，固定 commit `6c46345a727e7c1b57e0dacecb4fe063d88ee8dc`，MIT。仓库根 LICENSE 和原版权保留；移植与改动映射见 docs/research/REUSE-MAP.md。

仅只读参考的 Agent Session Manager Desktop、Multi CLI Studio、CodexBar 为 MIT；AionUi/AionCore 为 Apache-2.0，具体快照与阅读路径见 references.lock.json。当前没有复制其代码。

pnpm-lock.yaml 和 src-tauri/Cargo.lock 固定继承依赖。已导出 [依赖许可元数据](docs/research/dependency-licenses.json)，共 1089 条（pnpm production graph + Cargo Windows 过滤元数据），没有缺失 license 字段。这只是包元数据，完整分发许可文本、NOTICE、字体/图标与最终实际打包资产审核仍是发布前检查项，不能以本摘要替代。

MultiCLI 新应用图标为本项目 `public/multicli.svg` 生成，当前 bundle.icon 使用独立 multicli-icons 文件，不沿用上游应用图标。原上游图标保留在继承源码用于溯源，不作为 MultiCLI 应用标识。
