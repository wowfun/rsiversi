# 从源码安装

[English](source-install.md)

本教程带你获取 Git 源码、构建应用并打开 Web。
脚本不会安装系统组件，也不会修改机器的安全设置，不支持源码 ZIP 压缩包。
首次构建需要编译 Rust 和 Web 依赖，之后启动会直接使用已发布的应用。
请为首次 release 构建预留足够时间和磁盘空间；笔记本上可能超过一小时。
标准错误会显示目标、release/debug 模式、阶段、命令和 `build.log` 路径；长阶段每 15 秒报告一次状态。`--verbose` 同时显示工具输出，标准输出保留最终 JSON 结果。后续构建会复用编译缓存，启动 Web 不会重新编译。

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
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1 setup web
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1 build web
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1 web
```

执行策略参数只对当前进程生效。组织策略仍可能拒绝执行，脚本不会更改策略。
`setup` 会准备仓库指定的 Rust 工具链和 WASM 目标，
并把 pnpm、wasm-bindgen 放在仓库自己的工具目录中。可以重复执行，无需 Python 或 Make。

## Linux、WSL 或 Windows Git Bash

<!-- rsi-example id=install-bash platform=git-bash -->
```bash
bash scripts/rsiversi.sh setup web
bash scripts/rsiversi.sh build web
bash scripts/rsiversi.sh web
```

Git Bash 使用原生 Windows 工具和数据目录，不会进入 WSL。
mintty 可以启动 Web；全屏 TUI 需要在 Windows Terminal 中运行 Git Bash。

## 选择构建目标

`doctor`、`setup`、`build` 和 `path` 的第一个参数可以是 `tui`、`web` 或 `desktop`。省略时选择 **tui**；上面的示例显式选择 Web。

| 目标 | 发布内容 | 额外依赖 |
| --- | --- | --- |
| `tui` | `rsi` 中的 TUI、CLI、Headless、ACP、HTTP API 和管理命令 | 只需 Rust 和原生编译器，无需 Node、WASM 或 musl |
| `web` | 原生 `rsi`、配套 Web 资源和 Worker | Node/npm、指定版本 pnpm 和 wasm-bindgen、WASM 目标；Linux 还需要 musl |
| `desktop` | Web 配套包及 `rsi-desktop` | Linux 需要 GTK 3 / WebKitGTK 4.1 开发库；Windows 启动时需要 WebView2 Runtime |

Desktop 依次执行 `setup desktop`、`build desktop`、`desktop`。原生 TUI 使用 `setup tui`、`build tui`，再在 Windows Terminal 或 Linux 终端中执行 `tui`。所有目标都保留完整原生 catalog 和内置 Profile；构建目标不会按运行时 Profile 裁剪依赖。

Linux Web/Desktop 默认包含 SSH helper。Linux TUI 通过 `setup tui --with-ssh-helper` 和 `build tui --with-ssh-helper` 选择包含它。helper 始终使用 release，即使主应用选择 debug。未包含时无法使用产品 SSH target，但普通 SSH 登录后运行本地 TUI 不受影响。Windows 拒绝此 helper 参数。

每个目标有独立的当前产物和编译缓存。需要共享 Linux Service Host 的客户端必须使用同一 family：`build web` 后用 `web` 和 `tui --from web`；`build desktop` 后用 `desktop`、`web --from desktop` 或 `tui --from desktop`。Windows 在同一 Host 上切换界面前仍需停止已有界面。入口不会自动回退到其他目标。

## 检查、升级与查找产物

`doctor` 只检查前置依赖，不安装、不构建。PowerShell 按上面的调用方式执行
`scripts/rsiversi.ps1 doctor web`，Bash 执行 `bash scripts/rsiversi.sh doctor web`。
根据诊断补齐依赖后重试。

使用 `--help` 或 `-h` 查看总帮助，使用 `build --help` 查看构建选项。
`doctor`、`setup`、`build` 和 `path` 的帮助在 setup 前即可使用，无需已有产物。
`web --help`、`tui --help` 和 `desktop --help` 由已构建应用提供，需要先成功构建。

<!-- rsi-example id=install-help-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1 --help
```

<!-- rsi-example id=install-help-bash platform=git-bash -->
```bash
bash scripts/rsiversi.sh build --help
```

`build` 默认生成 TUI release 产物，Web 开发可用 `build web --debug`。可选的新绝对输出目录会发布到托管缓存之外，不改变当前托管产物。
构建失败会保留上一次成功产物。升级前先退出 RSIversi、更新源码，
再执行 setup 和 build，重新启动 Web 后使用新版本。

`path web` 输出当前 Web 产物目录（单独 `path` 选择 TUI），里面包含 Windows 的 `rsi.exe`
（Linux 为 `rsi`）、`assets` 和构建回执。
你可以直接给这个程序传入 `web` 启动，请保留版本目录的完整布局。
启动和 path 命令不调用 Cargo、Node 或构建器。应用运行期间，其托管版本目录不会被回收。

完整参数与发布规则见[源码入口契约](../../apps/devtools/README.md)。

接下来[完成第一次对话](first-conversation.zh.md)。
构建失败和长路径问题见[故障排查](troubleshooting.zh.md)。
