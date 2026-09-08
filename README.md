# RSIversi

RSIversi is a pre-release Rust workspace for composing independently usable
runtime products from ordinary `rsi-meta` plugins.

The repository-wide ownership and dependency rules live in
[the architecture](docs/architecture.md). Each product owns its public contract,
setup, security, and verification documentation below its own subtree.

Development builds optimize the SHA-256 dependency used to verify complete
executable artifacts. This keeps local startup practical with full debug
information while preserving the exact build-identity check and its deadlines.
