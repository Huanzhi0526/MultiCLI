# 来源追踪

| 目标 | 来源 | 固定 commit | 用途与修改 |
|---|---|---|---|
| app/ 上游继承树（不含新增 MultiCLI 文件与用户方案） | https://github.com/zhukunpenglinyutong/desktop-cc-gui | 6c46345a727e7c1b57e0dacecb4fe063d88ee8dc | 完整 Git 派生；保留 MIT LICENSE 和原版权。S00 修改启动、数据目录、身份、构建配置；git diff 可逐文件追溯 |
| src-tauri/src/multicli_bootstrap.rs、src/multicli/、scripts/multicli/、docs/contracts/ | 本项目新增 | 本次 S00 提交待验收 | 本地脚手架/契约，不来源于其他参考仓库 |

其余五个参考目前仅阅读，没有代码移植。后续任何复制需逐目标文件增加来源 commit、原路径及修改摘要。上游图标不会作为 MultiCLI 最终品牌资产发布；第三方字体/图标许可在依赖声明中单独核对。
