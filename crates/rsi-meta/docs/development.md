# Developing rsi-meta

## Prerequisites

Use the Rust toolchain pinned by [`rust-toolchain.toml`](../../../rust-toolchain.toml). Root workspace commands cover the four product crates and `rsi-xtask`; standalone plugin and fixture workspaces are exercised by the conformance xtask.

## Daily checks

Run the narrowest owning package checks while iterating. Before handing off a cross-package change, run:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets
cargo xtask rsi-meta code-health
cargo xtask verify-docs
```

Run the product-wide standalone checks when a package, ABI, schema, fixture, or shared contract changes:

```sh
cargo xtask rsi-meta conformance
```

The [testing policy](testing.md) maps changed surfaces to additional failpoint and release checks.
