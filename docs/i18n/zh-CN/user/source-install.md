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

安装器只构建 release 模式的 `rsi`，并将其 `bin` 目录加入用户 PATH。
安装后重新打开终端，无需选择 UI。

<!-- rsi-example id=install-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1
```

<!-- rsi-example id=install-bash platform=linux -->
```bash
bash scripts/rsiversi.sh
```

<!-- rsi-example id=install-launch platform=all -->
```bash
rsi
```

`--prefix DIR` / `-Prefix DIR` 选择安装根目录，`--binary FILE` / `-Binary FILE`
使用你明确信任的已有可执行文件。安装器拒绝覆盖现有安装。
`--no-path` / `-NoPath` 不修改 PATH，供隔离验证使用。安装不会打开配置或状态。

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

Desktop 使用 `node apps/devtools/build.mjs desktop ABSOLUTE_NEW_PACKAGE
--web-package ABSOLUTE_WEB_PACKAGE` 构建，再用 `rsi plugin install --local`
安装这两个包目录。Windows 启动时需要 WebView2；Linux 构建还需要
`libgtk-3-dev libwebkit2gtk-4.1-dev`。Desktop 包固定其精确的 Web 依赖。
SSH helper 在 Linux 上通过 `build.mjs ssh-helper` 单独构建；先准备相同 CPU 的
musl target 和编译器，再通过 `rsi` 安装。

不加 `--debug` 时构建 release 包。默认要求新的输出目录；`--replace` 用于
明确选择的源码输出。详见[构建工具契约](../../../../apps/devtools/README.md)和
[开发指南](../../../development.md)。

Windows 和 Linux 上的 TUI 与 Web 共享常驻 Host。关闭 UI 只断开附件。
`rsi web stop` 停止浏览器 listener，`rsi host stop` 停止共享服务。
接下来可阅读[第一次对话](first-conversation.md)。

## 升级与导入已有数据

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
