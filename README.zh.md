# RSIversi

[English](README.md)

RSIversi 是一个预发布的 Rust 工作空间，通过普通的 `rsi-meta` 插件组合可独立使用的运行时产品。

想使用 RSI，请从[完成第一次对话](crates/rsi/docs/user/first-conversation.zh.md)开始，
按照教程安装源码、配置模型，并恢复已保存的对话。

仓库级职责和依赖规则见[架构](docs/architecture.md)。
各产品在自己的目录下维护公开契约、安装、安全和验证文档。

开发者可阅读[开发快速开始](docs/quickstart.md)，构建并启动隔离的 TUI、Web 环境或配套的 Linux 桌面发行。
开发构建会优化用于验证完整可执行文件的 SHA-256 依赖，在保留完整调试信息、构建身份校验和期限约束的同时改善本地启动速度。
