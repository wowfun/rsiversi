# 从源码安装

[English](source-install.md)

本教程带你获取 Git 源码、构建应用并打开 Web。
脚本不会安装系统组件，也不会修改机器的安全设置，不支持源码 ZIP 压缩包。
首次构建需要编译 Rust 和 Web 依赖，之后启动会直接使用已发布的应用。
请为首次 release 构建预留足够时间和磁盘空间；笔记本上可能超过一小时。
每个构建阶段都会打印 `build.log` 的位置。后续构建会复用编译缓存，启动 Web 不会重新编译。

## 准备机器

Windows 要求 Windows 11 24H2 或更新版本、x64，以及固定本地 NTFS 磁盘上的较短仓库路径。
请先安装 Git for Windows、Rustup、Node.js 22.18 或更新版本，
以及 Visual Studio Build Tools 的 **Desktop development with C++（使用 C++ 的桌面开发）**，
包含 MSVC x64 和 Windows SDK。安装后重新打开 PowerShell。

Linux 或 WSL 需要 Git、Rustup、带 npm 的 Linux Node.js 22.18 或更新版本、
C 编译器和 musl 工具。Ubuntu 的系统依赖包括
`build-essential musl-tools`。WSL 必须使用 Linux Node，
不能使用从 PATH 继承的 Windows Node。本教程的源码发布入口暂不支持原生 macOS。

克隆仓库：

<!-- rsi-example id=install-clone platform=all -->
```bash
git clone https://github.com/wowfun/rsiversi.git
cd rsiversi
```

## Windows PowerShell 5.1 或 7

依次执行：

<!-- rsi-example id=install-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\rsi.ps1 setup
powershell -NoProfile -ExecutionPolicy Bypass -File .\rsi.ps1 build
powershell -NoProfile -ExecutionPolicy Bypass -File .\rsi.ps1 web
```

执行策略参数只对当前进程生效。组织策略仍可能拒绝执行，脚本不会更改策略。
`setup` 会准备仓库指定的 Rust 工具链和 WASM 目标，
并把 pnpm、wasm-bindgen 放在仓库自己的工具目录中。可以重复执行，无需 Python 或 Make。

## Linux、WSL 或 Windows Git Bash

<!-- rsi-example id=install-bash platform=git-bash -->
```bash
bash rsi.sh setup
bash rsi.sh build
bash rsi.sh web
```

Git Bash 使用原生 Windows 工具和数据目录，不会进入 WSL。
mintty 可以启动 Web；全屏 TUI 需要在 Windows Terminal 中运行 Git Bash。

## 检查、升级与查找产物

`doctor` 只检查前置依赖，不安装、不构建。PowerShell 按上面的调用方式执行
`rsi.ps1 doctor`，Bash 执行 `bash rsi.sh doctor`。
根据诊断补齐依赖后重试。

`build` 默认生成 release 产物，开发时可用 `build --debug`。
构建失败会保留上一次成功产物。升级前先退出 RSI、更新源码，
再执行 setup 和 build，重新启动 Web 后使用新版本。

`path` 输出当前产物目录，里面包含 Windows 的 `rsi.exe`
（Linux 为 `rsi`）、`assets` 和构建回执。
你可以直接给这个程序传入 `web` 启动，请保留版本目录的完整布局。
启动和 path 命令不调用 Cargo、Node 或构建器。应用运行期间，其托管版本目录不会被回收。

接下来[完成第一次对话](first-conversation.zh.md)。
构建失败和长路径问题见[故障排查](troubleshooting.zh.md)。

