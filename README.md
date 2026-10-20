<p align="center">
  <img src="apps/web/public/logo.svg" alt="Psychevo" width="160">
</p>

English | [中文](README.zh.md)

# RSIversi

RSIversi is a pre-release Rust workspace for composing independently usable
runtime products from ordinary `rsi-meta` plugins.

To use RSIversi, start with [your first conversation](docs/user/first-conversation.md).
It covers source installation, model setup and returning to saved conversations.

The repository-wide ownership and dependency rules live in
[the architecture](docs/architecture.md). Each product owns its public contract,
setup, security, and verification documentation below its own subtree.

Follow the [development guide](docs/development.md) to run the standard debug
product on Windows or Linux/WSL, with Web hot updates. Explicit keyless fixture
tools are available on Linux/WSL. Build-only and Desktop workflows are covered in
[Install from source](docs/user/source-install.md).

Development builds optimize the SHA-256 dependency used to verify complete
executable artifacts. This keeps local startup practical with full debug
information while preserving the exact build-identity check and its deadlines.
