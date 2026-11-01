# Sol 独立验收模板

- task_id / revision：
- 结论：ACCEPTED / REWORK_REQUIRED / BLOCKED
- 覆盖范围：
- base_commit：
- 受测源码 manifest / SHA256 位置：
- OS / CLI / 工具链 / app 配置：
- 时间与时区：

## 独立验证

| 验收 ID | 独立输入和操作 | 预期 | 实际 | PASS/FAIL/NOT_RUN | 证据 |
|---|---|---|---|---|---|

## 缺陷和返工

| 缺陷 ID | 严重度 | 复现步骤 | 预期/实际 | 修复边界 |
|---|---|---|---|---|

## 范围、数据与来源检查

记录授权文件范围、原生历史只读、敏感数据排除、复制代码来源和许可证。不能只写“检查通过”而无检查对象。

## Git 状态

- 实现提交：PENDING / 实际 commit
- 远端同步：PENDING / 实际 remote branch 与 commit
- CI：NOT_RUN / RUNNING / PASS / FAIL + 链接
- 安装包（适用时）：源码 commit / 文件路径 / SHA256

如提交字段需事后回填，单独记录最终 commit 映射，避免要求验收文件包含自身未来 commit hash。文档回填不改变受测源码。

## 剩余限制与下一步

明确未测项和可选项，不把未测项藏在 ACCEPTED 后面。
