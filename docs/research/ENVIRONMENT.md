# 环境信息

当前构建方法以[DEVELOPMENT](../DEVELOPMENT.md)、package.json、rust-toolchain.toml及锁文件为准。

0.1.0本机观察：Windows11企业版x64 10.0.26300；Ryzen7 7735H，8核16线程，约16GB RAM；Node24.16.0、pnpm10.29.3、Rust1.99.1；MSVC14.44.35207与SDK10.0.26100.0；PowerShell7.6.5、Python3.10.2；WebView2曾观察为151.0.4129.107。
这些是带日期的2026-11-04基线快照，组件可能更新，不是当前所有机器的事实。

本机scratch/tools保存隔离Rust/Cargo/Corepack/Perl工具缓存；enter-env脚本只在当前进程启用，不要求其他设备拥有相同绝对路径。
远程开发者通过本机工具或Windows CI恢复依赖，不能把本机安装/登录状态当作云端前提。

Git源仓库为Huanzhi0526/MultiCLI；upstream只作来源追踪。不要将Git作者、CLI登录、Git传输认证和GitHub连接权限混为同一项。
