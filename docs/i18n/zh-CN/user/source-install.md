# 从源码安装

[English](../../../user/source-install.md)

核心只安装一次。TUI、CLI、Headless、ACP 和管理功能是静态链接的插件，
统一通过 `rsi` 使用。Web、Desktop 和 SSH helper 是独立包；增添或替换它们
不会重新编译或替换核心可执行文件。

## 准备机器

安装 Git 和仓库固定的 Rust 工具链。Windows 要求 Windows 11 24H2 或更新版本、
x64、固定本地 NTFS 磁盘上的较短路径，以及 Visual Studio Build Tools 的
Desktop development with C++、MSVC x64 和 Windows SDK。TUI 需要 Windows Terminal。
Linux/WSL 需要 C 工具链；Ubuntu 安装 `build-essential pkg-config libdbus-1-dev`。
本教程不涵盖原生 macOS。

<!-- rsi-example id=install-clone platform=all -->
```bash
git clone https://github.com/wowfun/rsiversi.git
cd rsiversi
```

## 安装核心

安装器构建 release 模式的 `rsi` 与独立卸载器 `rsi-uninstall`，并将其 `bin` 目录加入用户 PATH。
安装后重启终端应用，无需选择 UI。

Windows 上使用 PowerShell：

<!-- rsi-example id=install-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\install.ps1
```

也可以在 Windows Git Bash 中执行：

<!-- rsi-example id=install-git-bash platform=git-bash -->
```bash
bash scripts/install.sh
```

这两个 Windows 命令使用同一个原生安装器，默认安装到 `%LOCALAPPDATA%\RSIversi`。
安装后重启终端应用，让新的 Git Bash 会话继承更新后的 Windows 用户 PATH。

Linux 或 WSL：

<!-- rsi-example id=install-bash platform=linux -->
```bash
bash scripts/install.sh
```

<!-- rsi-example id=install-launch platform=all -->
```bash
rsi
```

`--prefix DIR` / `-Prefix DIR` 选择安装根目录，`--binary FILE` / `-Binary FILE`
必须搭配 `--uninstaller FILE` / `-Uninstaller FILE`，使用你明确信任的预构建主程序和卸载器。
源码安装一次构建两个程序；显式二进制安装不调用 Cargo。独立卸载器和对应平台的卸载脚本
保存在 `<prefix>/libexec/rsi`，不加入 PATH。安装器在构建前检查目标，遇到已有文件、目录
或链接会立即退出。安装目录及其父级不能经过符号链接或 junction；请显式传入解析后的真实目录。若已有 `rsi`，提示会显示完整安装路径和升级命令；请按
[下方的升级步骤](#升级与导入已有数据)操作，或选择另一个空的安装目录。
`--no-path` / `-NoPath` 不修改 PATH，供隔离验证使用。安装不会打开配置或状态。

## 卸载与清理运行数据

先把下方 `prefix` 改为实际安装目录，再预览卸载范围；使用自定义安装目录时不能省略该参数。

<!-- rsi-example id=uninstall-preview-windows platform=windows -->
```powershell
$prefix = Join-Path $env:LOCALAPPDATA "RSIversi"
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/uninstall.ps1 -Prefix $prefix -DryRun
```

<!-- rsi-example id=uninstall-preview-linux platform=linux -->
```bash
prefix="$HOME/.local"
bash scripts/uninstall.sh --prefix "$prefix" --dry-run
```

移除预览参数后执行卸载。即使删除源码目录，也可以使用 `<prefix>/libexec/rsi`
中的对应脚本；它默认选择所在安装目录。Git Bash 委托原生 PowerShell。卸载不运行
待删除的 `rsi`，不需要 Cargo，也不下载工具。Windows 实际卸载使用经过摘要校验的临时副本，dry-run 原地运行已安装工具；
Linux 直接运行卸载器。

普通卸载保留配置、状态和缓存，包括共享包及 Host 运行时副本。`--purge` / `-Purge`
会展示最终配置、状态和缓存目录 并确认后清理保存的凭据、会话、生成状态、WebView 数据和缓存。
其他安装和开发版本也可能使用这些目录；非交互确认使用 `--yes` / `-Yes`。
用户编写的启动与 settings 配置、AGENTS.md、skills、agents、Profiles、自定义 presets
及 addon 源码继续保留，无关文件和外部引用也不会被删除。完整规则见
[卸载契约](../../../../crates/rsi/installation/README.md)。

使用 `--config-home`、`--state-home`、`--cache-home`（PowerShell 对应 `-ConfigHome`、
`-StateHome`、`-CacheHome`）直接选择最终目录，不加载配置。属于本安装的 Host 会正常停止；
嵌入式 Host、使用中的包和锁定文件的客户端需要先关闭再重试。部分失败会保留卸载工具
并报告剩余工作；用于并发协调的空锁文件会保留。

安装记录缺失或损坏时，需提供明确信任的外部 `--uninstaller FILE` / `-Uninstaller FILE`
并使用 `--force` / `-Force`。Force 只授权清理展示的固定安装目标，不绕过锁、待恢复事务、
路径检查或停机失败。PATH 只撤销有可靠记录且仍精确匹配的新增项；共享 bin 仍有其他程序时
保留 PATH。符号链接或只读的 POSIX 启动文件需要手动配置。

安装文件已不存在时，重复普通卸载成功退出。卸载后的再次 purge 需要从可信 core 包中
另取 `rsi-uninstall.exe`（Windows）或 `rsi-uninstall`（Linux），复制到目标安装目录外，
通过上述外部工具参数指定；仍传入相同 prefix 和 home 参数。core 包的构建方法见下方升级步骤，
获取工具与卸载执行是两个独立步骤；卸载器不会自动构建或下载。无需恢复主程序。

## 添加 Web 或 Desktop

在[启动配置](../../../../crates/rsi/core/docs/launch.md)中设置 `[repository]` 后，
使用普通产品命令添加界面。发布信任规则见[包契约](../../../../crates/rsi/packages/README.md)；测试密钥不能作为官方发布身份。

<!-- rsi-example id=install-plugins platform=all -->
```bash
rsi plugin install web
rsi web
rsi plugin install desktop
rsi desktop
```

从本地源码构建包时，安装 Node.js 22.18 或更新版本、固定版本的 pnpm、
WASM target 和匹配的 wasm-bindgen。WSL 必须使用 Linux Node。

<!-- rsi-example id=install-web-tools platform=all -->
```bash
npm install --global pnpm@12.6.0
pnpm -C apps/web install --frozen-lockfile --ignore-scripts
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --locked --version 0.2.127
```

构建 Web 包并安装到同一组产品目录：

<!-- rsi-example id=install-web-windows platform=windows -->
```powershell
node apps/devtools/build.mjs web "$PWD/.local/packages/web"
rsi plugin install --local "$PWD/.local/packages/web"
rsi web
```

<!-- rsi-example id=install-web-linux platform=linux -->
```bash
node apps/devtools/build.mjs web "$PWD/.local/packages/web"
rsi plugin install --local "$PWD/.local/packages/web"
rsi web
```

Desktop 使用 `node apps/devtools/build.mjs desktop ABSOLUTE_NEW_PACKAGE` 构建，再用 `rsi plugin install --local`
安装这两个包目录。Windows 启动时需要 WebView2；Linux 构建还需要
`libgtk-3-dev libwebkit2gtk-4.1-dev`。Desktop 与 Web 可以独立更新；Desktop
会在打开窗口前检查实际加载文档的 bridge 版本。
SSH helper 在 Linux 上通过 `build.mjs ssh-helper` 单独构建；先准备相同 CPU 的
musl target 和编译器，再通过 `rsi` 安装。

不加 `--debug` 时构建 release 包。默认要求新的输出目录；`--replace` 用于
明确选择的源码输出。详见[构建工具契约](../../../../apps/devtools/README.md)和
[开发指南](../../../development.md)。

Windows 和 Linux 上的 TUI 与 Web 共享常驻 Host。关闭 UI 只断开附件。
`rsi web stop` 停止浏览器 listener，`rsi host stop` 停止共享服务。
接下来可阅读[第一次对话](first-conversation.md)。

## 升级与导入已有数据

安装器只负责首次安装，重复运行不会升级核心。从本地源码升级时，先使用现有
构建工具准备 core 包（此构建工具需要 Node.js 22.18 或更新版本，安装器本身不需要）：

<!-- rsi-example id=upgrade-core-source platform=all -->
```bash
node apps/devtools/build.mjs core "$PWD/.local/packages/core" --replace
rsi core upgrade --local "$PWD/.local/packages/core"
```

使用自定义安装目录时，应调用安装器提示中的完整可执行文件路径，而不是 PATH
上的 `rsi`，并保留该安装使用的全局 home 选项。`--local` 接受包含 `package.json`
的包目录绝对路径，不能直接传入 `target/release/rsi` 或 `rsi.exe`。
[构建工具契约](../../../../apps/devtools/README.md)也介绍了如何直接打包已构建的
主程序和卸载器，无需再次调用 Cargo。有安装记录时，core、卸载器、卸载脚本及记录通过
同一可恢复事务更新，缺少兼容卸载工具的包会被拒绝。无记录的旧安装继续使用单主程序升级；
旧程序执行首次升级不会自动建立安装记录。

包清单 format 2 需要新的 core 可执行文件。已有 format-1 包仓库应先把新二进制
安装到另一个空目录，停止旧 Host 和应用，再用新二进制及相同的 home 选项运行
`plugin list`。通过一次 `plugin uninstall NAME...` 移除全部旧选择，包括 `core`。
旧租约释放后运行 `plugin gc`，再安装新包。保留配置、状态和信任记录；重置不能
代替这项迁移。旧二进制不能安装 format-2 包。

先停止 Host，再把 `--reset-config`、`--reset-state` 或两者放在常用应用命令之前。
重置会保留备份、凭据和已安装包。例如 `rsi --reset-config --reset-state tui`
使用新的受管配置和状态启动 TUI。查看帮助不会执行重置。

使用可执行文件内固定的仓库身份运行 `rsi core upgrade`，或指定本地包：
`rsi core upgrade --local ABSOLUTE_CORE_PACKAGE`。命令先暂存并探测通过准入的
可执行文件，再停止所选 Host、替换核心，最后确认新 Host 已就绪。安装目录保留
旧可执行文件和回执。操作中断时，使用相同的全局 home 选项运行
`rsi core recover`；如果 `rsi` 文件名暂时不存在，可直接调用保留的
`.rsi-previous-…` 可执行文件。配置镜像地址时必须保留固定的仓库名称和公钥；
没有内置仓库身份的源码构建须使用 `--local`。Windows 升级要求当前用户拥有的
私有安装目录；遇到共享目录会拒绝操作，不会修改其 ACL。

显式迁移数据前，先停止来源和目标两个 Host。全局选项指定目标目录，import
后面的选项指定旧目录：

```text
rsi --config-home NEW_CONFIG --state-home NEW_STATE import --config-home OLD_CONFIG --state-home OLD_STATE --dry-run
rsi --config-home NEW_CONFIG --state-home NEW_STATE import --config-home OLD_CONFIG --state-home OLD_STATE
rsi --config-home NEW_CONFIG --state-home NEW_STATE import recover
```

dry-run 不创建目录或锁，也不修改权限。它只预览导入；实际操作会持有独占租约，
重新执行检查。
导入会暂存受管配置和状态，并保留目标旧数据的备份，不重写外部路径或转换数据库
schema。可选包需要单独安装。尚未完成的导入会阻止普通启动，直到恢复完成。
[启动契约](../../../../crates/rsi/core/docs/launch.md) 规定可导入路径、大小限制和恢复行为。
