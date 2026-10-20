<p align="center">
  <img src="apps/web/public/logo.svg" alt="RSIversi" width="160">
</p>

[English](README.md) | 简体中文

# RSIversi

RSIversi 是基于 Rust 的可扩展 Agentic Harness（智能体任务运行框架），
采用 Everything is a plugin（一切皆插件）的架构。

接入模型服务，在项目中与 Agent（智能体）协作，并按任务调整工具、上下文和运行时组合。
你可以使用现成应用，也可以用可独立使用的 Rust 组件组合自己的产品。

RSIversi 目前处于**预发布阶段**。API、插件契约和保存格式都可能变化，不保证兼容性。

## 在工作区中与 Agent 协作

- 通过模型调用和工具完成任务，包括文件访问、编辑和命令执行，具体取决于所选权限和平台支持。
- 用项目指令和可复用的 Skills（技能）指导 Agent，按角色定义和工具配置向子 Agent 分派任务。
- 持久保存对话历史，恢复已保存的会话。模型上下文由已持久化的会话记录构建。

你可以选择 [Web 界面](apps/serve/README.md#local-web-launch)、
[全屏 TUI](apps/terminal/README.md) 或
[Linux/Windows 桌面应用](apps/desktop/README.md)。
脚本调用或与其他客户端集成时，可使用[无界面模式（Headless）](apps/terminal/docs/line-application.md)
或 [ACP](apps/acp/README.md)。

可选的[工作流](examples/rsi/workflow/README.md)通过 JavaScript 协调 Agent 任务。
使用前需要在宿主组合配置（Host Profile）中显式启用 Node 运行时，并选择 `workflow` Agent 预设。

## Everything is a plugin

模型适配器、Agent 内核、上下文构建器、会话存储和应用的运行时组件，
都是普通的 `rsi-meta` 插件。Profile 负责组合这些实现，Agent 预设选择 Agent 可用的贡献项。
自定义实现与内置实现使用相同的公开契约。

插件是运行时生命周期与组合的单位。工具、命令和提示词片段是由插件注册的贡献项；
协议定义和纯逻辑仍是普通库。运行时基础层因此独立于模型适配器、Agent 语义和应用策略。
具体契约见[组合基础层](crates/rsi-meta/README.md)。

## 快速开始

请从 Git 工作副本构建。先安装 Git、Rustup、Node.js 22.18 或更新版本，
并按照[源码安装教程](docs/i18n/zh-CN/user/source-install.md)准备所在平台的编译依赖。

<!-- rsi-example id=install-clone platform=all -->
```bash
git clone https://github.com/wowfun/rsiversi.git
cd rsiversi
```

Windows 使用 Windows 11 24H2 或更新版本的 x64 系统，
将仓库放在固定本地 NTFS 磁盘上，然后在 PowerShell 中执行：

<!-- rsi-example id=install-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1 setup web
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1 build web
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1 web
```

Linux、WSL 或 Windows Git Bash：

<!-- rsi-example id=install-bash platform=git-bash -->
```bash
bash scripts/rsiversi.sh setup web
bash scripts/rsiversi.sh build web
bash scripts/rsiversi.sh web
```

首次构建需要编译原生应用和 Web 依赖，可能耗时较长。后续启动直接使用已构建的应用。
Web 会打开一次性登录链接；使用期间请保持启动它的终端打开。

在 **Settings → Models & access** 中，先保存凭据，再配置对应的模型服务，最后选择默认模型。
随后新建对话并选择工作目录。[完成第一次对话](docs/i18n/zh-CN/user/first-conversation.md)
会引导你完成配置，并说明如何返回已保存的对话。

Windows 当前支持对话、模型配置、历史记录和原生文件访问。
受限命令执行及浏览器与自动化功能（Browser/Automation）需要 Linux/WSL。
Windows 上使用同一个 Host 时，切换界面前需先停止原界面。
详情见[平台限制与排障](docs/i18n/zh-CN/user/troubleshooting.md)。

## 扩展 RSIversi

- 添加项目指令和[技能](crates/rsi-agent/workspace-context/README.md)，让 Agent 遵循可复用的操作流程，无需编写运行时插件。
- 通过可选的 [MCP 集成](crates/rsi-mcp/README.md)接入外部工具和资源。
- 用 [Agent 预设](crates/rsi-agent/presets/README.md)定制 Agent 的贡献项，
  用 [Host 和 Application Profile](crates/rsi/docs/subsystems/composition-and-profiles.md)组合服务与应用。
- 将 Rust 插件作为源码组件静态链接，或编写原生工具扩展（addon）。
  [扩展生成器](crates/tools/rsi-xtask/README.md)可在 Linux/WSL 上为这两种方式创建独立工作空间。
  源码组件重新构建后生效；原生扩展使用显式的 Native ABI 和 Portable 契约。

如果要构建自己的 Rust 应用，可以通过 [Host SDK](crates/rsi-host/README.md)
显式注册所需插件，并用选定的组件启动 Profile。

## 开发

[开发指南（英文）](docs/development.md)介绍了如何在 Windows 或 Linux/WSL 上运行标准产品的 debug 构建，
并使用 Web 热更新。Linux/WSL 还提供显式 fixture 工具，用于无需密钥的检查。仅构建和桌面开发见
[源码安装教程](docs/i18n/zh-CN/user/source-install.md)。

[仓库架构（英文）](docs/architecture.md)定义职责归属和依赖方向。
各产品在自己的目录下维护公开契约、安装、安全和验证文档。
