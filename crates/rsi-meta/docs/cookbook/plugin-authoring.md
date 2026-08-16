# Author a native rsi-meta plugin

This tutorial adds a trusted Rust `cdylib` package under `plugins/rsi-meta/`. Read the [security boundary](../security.md) before choosing native execution.

## 1. Create the package

Configure the library and depend on the safe SDK:

```toml
[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
rsi-meta-plugin = { path = "../../../crates/rsi-meta/plugin" }
```

Implement `Plugin` and export the entry symbol:

```rust
use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::Lane;

struct Example {
    host: Host,
}

impl Plugin for Example {
    type Error = std::convert::Infallible;

    fn create(host: Host) -> Result<Self, Self::Error> {
        Ok(Self { host })
    }

    fn on_frame(&mut self, lane: Lane, frame: &[u8]) -> Result<(), Self::Error> {
        let _ = (&self.host, lane, frame);
        Ok(())
    }
}

rsi_meta_plugin::export_plugin!(Example);
```

The [plugin package README](../../plugin/README.md) describes the ABI, threading, ownership, and failure contract.

## 2. Declare the package

Add `plugin.toml` beside the Cargo manifest. Declare package identity, host API, target-qualified artifacts, provided/injected contracts, capabilities, and a configuration schema. Use the [plugin schema](../../../../schemas/rsi-meta/plugin.schema.json) and [configuration reference](../subsystems/configuration.md) rather than copying a fixture manifest blindly.

## 3. Handle lifecycle and streams

Treat `prepare` as fallible shadow work and emit its same-generation terminal acknowledgement. Do not publish services or perform irreversible effects before commit. Drain generation-owned work before acknowledging retirement.

Decode service frames according to the [plugin frame schema](../../../../schemas/rsi-meta/plugin-frame.schema.json). Enforce sequence, credit, and terminal behavior described by the [protocol reference](../subsystems/protocols.md); retry `WouldBlock` only after progress and treat `Closed` as permanent.

## 4. Verify the package

Give the Cargo package a sibling README for its consumers, then run:

```sh
cargo fmt --manifest-path plugins/rsi-meta/my-plugin/Cargo.toml --check
cargo clippy --locked --manifest-path plugins/rsi-meta/my-plugin/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path plugins/rsi-meta/my-plugin/Cargo.toml
```

Use [`plugin-testkit`](../../../../fixtures/rsi-meta/plugin-testkit/README.md) for black-box ABI behavior. Include a package in product conformance only when it is a maintained release or conformance artifact, then run `cargo xtask rsi-meta conformance` from the repository root.
