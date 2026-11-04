# 开发与构建

以下是0.1.0已使用的工具链基线，不代表要求自动升级到最新版本。正式Git根就是包含package.json的目录；用户本机为D:\Vibe Coding\MultiCLI\app，其他机器不需要相同父目录。

## 工具链

| 组件 | 基线 |
|---|---|
| 平台 | Windows 11 x64；原生构建使用MSVC |
| Node / pnpm | Node24.16.0；packageManager固定pnpm10.29.3，使用Corepack |
| Rust | rust-toolchain.toml固定1.99.1 |
| 原生依赖 | MSVC C++ Build Tools与Windows SDK、完整原生Perl（含IPC::Cmd与Locale::Maketext::Simple） |
| 桌面运行 | WebView2 Runtime |
| 构建器 | 锁文件中的Tauri CLI；Windows产物为NSIS用户级安装器 |

新的Windows机器需准备上述工具。Git自带的精简Perl可能缺模块，不能当作完整Perl。
不要上传工具安装目录，按锁文件恢复依赖即可。CLI账号和真实历史不是编译所需文件。

## 安装依赖与运行

在仓库根PowerShell执行：

```powershell
corepack pnpm install --frozen-lockfile --ignore-scripts
. ./scripts/multicli/enter-env.ps1
$env:MULTICLI_PROFILE = 'dev'
corepack pnpm dev
```

enter-env优先使用父目录scratch/tools里的本机工具缓存；没有该目录时使用机器自身PATH，不要求远程开发者复制这些缓存。
它只修改当前进程环境，Rust编译并发基线为2。测试时使用test或独立绝对数据根，避免让开发实验操作日常prod数据。

只看前端可在仓库根运行`corepack pnpm exec vite`；实际Tauri IPC、进程和数据库操作仍需原生应用。不要把浏览器预览当成原生功能验证。

## 按需检查

```powershell
./scripts/verify.ps1 -Scope Frontend
./scripts/verify.ps1 -Scope Rust
./scripts/verify.ps1 -Scope Test
./scripts/verify.ps1 -Scope Package
# 有需要时执行全部：
./scripts/verify.ps1 -Scope All
```

Frontend包含类型检查、Vite和平台边界检查；Rust是locked check；Test覆盖当前MultiCLI前端、领域、bootstrap与fixture保护；Package生成未签名NSIS包。
包装入口会标出实际执行和未执行范围。可以按具体修改运行更窄的Vitest/cargo过滤；纯文档修改做内容、链接和差异检查即可。
继承的全仓上游测试不全属于当前产品范围，不能拿一个过滤结果说成所有上游功能已验证。

安装器输出`src-tauri/target/release/bundle/nsis/`。再次构建通常复用target，依赖/工具链/配置变化会触发相应重编；不需要为普通维护先删除target。
冷编译Windows CI可能接近一小时；工作流当前没有Rust编译缓存。CI不登录用户CLI，不验证真实模型回复。
工作流在分支push、PR或手动触发时运行；单独推送版本标签不会重复构建，也不会自动发布Release。

## 其他设备与云端

Linux可修改文档、React/TypeScript并运行前端检查；ConPTY、Job Object、托盘和安装需要Windows。Windows CI见[工作流](../.github/workflows/multicli-windows.yml)。
不要把Linux原生构建结果当作Windows支持，也不要从其他机器复制整个node_modules或target作为源码交接。
协作方式见[CONTRIBUTING](../CONTRIBUTING.md)。

## 版本与构建来源

当前产品版本在package.json、src-tauri/tauri.conf.json、Cargo.toml与Cargo.lock中为0.1.0。
产品版本与插件SDK等独立包版本不同，不能机械改成同一个数字。
发版时同步CHANGELOG.md、docs/releases/与src/version/changelog.ts。产物记录源码commit、工具链、SHA256、平台及是否签名。
历史0.1.0已验证包对应的源码/摘要见[版本基线](releases/0.1.0.md)；文档或元数据整理不等于旧二进制重新构建。
