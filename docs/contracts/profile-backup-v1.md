# profile-backup-v1：本应用自有 profile 的备份与恢复（S07-R1）

状态：S07-R1 源码独立验收通过，整体安装与性能门禁待完成。范围只有一个 SQLite 数据库：本应用当前 profile 的
`multicli.db`。快照**包含本应用已索引的会话与消息内容**（这是用户自己要备份的数据），但**不含**任何
CLI 凭据、CLI 原始历史文件、工作区文件、浏览器存储或其他应用的数据。

## 1. 不变量

1. **范围由代码保证，不由用户勾选保证。** 导出只接受 `MultiCliStore` 已打开的连接，快照写入本
   profile 的 `backups/` 目录；库内没有指向 CLI 目录的表，备份过程也不读 CLI 目录。
2. **一致性与可验证性。** 备份用 `VACUUM INTO` 生成（包含 WAL 中已提交的内容），随后以只读连接执行
   `PRAGMA integrity_check`，再计算 SHA-256 与字节数写入清单。清单与文件任一不匹配，恢复入口禁用。
3. **恢复不在运行中替换数据库。** `multicli_profile_restore` 只写 `restore-pending.json`；真正的替换
   发生在**下次启动、数据库打开之前**。本次运行中不关闭/替换任何打开着的连接。
4. **失败不破坏原库。** 应用恢复前先把现有 `multicli.db`（及 `-wal` / `-shm`）复制到
   `backups/pre-restore-<stamp>-<id>/`；替换时把旧 `-wal` / `-shm` **移动**到同一目录，而不是删除；
   任何一步失败都把 sidecar 移回原位，主库文件保持原字节。校验和/完整性/版本任一不通过则不执行替换。
   完成记录 `last-restore.json` 是**提交点**：它写入失败时，已换入的新库会回滚到 pre-restore 副本并返回
   `io`，不会出现“已替换却报原库未变”。
5. **不降级。** 备份的 `databaseSchema` 大于当前构建的 `SCHEMA_VERSION` 时拒绝恢复：
   `plan` 阶段返回 `conflict`，`apply` 阶段返回 `invalid_argument`（标记已存在、文件可能被替换）。
   `apply` 会再次读取快照自身的 `schema_migrations`，防止计划后替换文件或降级应用绕过。更旧的
   schema 允许恢复，并在下次打开时走既有事务迁移。
6. **默认保留数据。** 卸载器默认不删除本 profile；只有用户勾选「删除应用数据」时才由 NSIS hook
   删除 `%LOCALAPPDATA%\MultiCLI`（见 `src-tauri/windows/nsis-hooks.nsh`）。

## 2. 目录与文件

```
<profile>/
  multicli.db                 # 本应用数据库（WAL）
  backups/
    multicli-YYYYMMDD-HHMMSS-<id>.db      # 快照
    multicli-YYYYMMDD-HHMMSS-<id>.db.json # 清单
    pre-restore-YYYYMMDD-HHMMSS-<id>/     # 被替换前的数据库副本
  backups/migrations/        # 迁移前的内部安全副本（不在用户备份列表显示）
  restore-pending.json        # 待执行的恢复（存在即会在下次启动时执行）
  last-restore.json           # 最近一次成功恢复的结果
```

## 3. 数据结构

`BackupManifest`（`camelCase`，`deny_unknown_fields`）：

| 字段 | 说明 |
|---|---|
| `manifestVersion` | 清单格式版本，当前 `1` |
| `createdAt` | UTC ISO-8601 |
| `appVersion` | 生成备份的构建版本（`CARGO_PKG_VERSION`） |
| `profile` | `dev` / `test` / `prod` |
| `databaseSchema` | 快照内 `schema_migrations` 的最大版本 |
| `fileName` | 快照文件名（纯文件名，不含路径） |
| `sizeBytes` / `sha256` | 文件字节数与校验和 |
| `source` | `manual` / `pre-restore` |
| `tables` | 快照内的表名（仅名字，无行数据），用于证明范围 |
| `scopeNote` | 固定的范围声明 |

`BackupRecord`：`{ manifest, path, integrity }`，`integrity` 为
`ok | missing | checksum-mismatch | unreadable | invalid-name`。

`ProfileDataView`：profile 事实（`profile`、`dataRoot`、`databaseFile`、`databaseBytes`、
`databaseSchema`、`expectedSchema`、`journalMode`、`backupDirectory`）+ `backups[]` +
`pendingRestore` + `lastRestore` + `scopeNote`。

`PendingRestore`：`{ planId, backupFileName, sha256, requestedAt, databaseSchema, note, failed? }`。
`RestoreOutcome`：`{ planId, restoredFrom, restoredSha256, appliedAt, previousCopy, databaseSchema }`；
`planId` 是每次计划的稳定 UUID，恢复已成功但 pending 文件删不掉时用它精确抑制重放；
`restoredSha256` 用于无 `planId` 旧记录的兼容去重；带 id 的新计划以计划 id 精确匹配。
`backupFileName` 必须是 `backups/` 下的单段纯文件名；`..`、绝对路径与 `C:` 驱动器前缀在
`list`、`plan`、`apply` 三处都会被拒绝。列表遇到非法清单时保留该行并标记 `integrity=invalid-name`，
不读取、不 hash 目录外文件。`failed=true` 表示上次启动尝试应用但失败：标记保留、数据库未替换，
界面显示失败而不是继续承诺“下次启动执行”。

## 4. IPC

| 命令 | 输入 | 输出 |
|---|---|---|
| `multicli_profile_data` | — | `ProfileDataView` |
| `multicli_profile_backup` | — | `BackupRecord` |
| `multicli_profile_restore` | `{ fileName }` | `PendingRestore`（只计划，不执行） |
| `multicli_profile_cancel_restore` | — | `()` |

失败返回真实 `AppError`：备份清单缺失 `not_found`；清单不可解析、校验和不符、非本应用数据库、
完整性检查失败为 `invalid_argument`；备份 schema 更新为 `conflict`。

## 5. 与迁移的关系

备份不改变迁移逻辑，迁移仍是 `store.rs` 的事务 + `VACUUM INTO` 迁移前备份。恢复一个旧 schema 的
快照后，下一次 `MultiCliStore::open` 会执行既有迁移；迁移失败会回滚并保留恢复前的数据库副本。

## 6. 未覆盖

- 备份不包含 WebView2 的 `webview/` 目录与 `diagnostics/` 导出文件；这些是可重建/可再导出的派生数据。
- 不提供定时自动备份、云同步或跨机器迁移。
- 恢复需要用户手动重启应用；界面明确说明这一点，不假装已生效。
