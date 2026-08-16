# rsi-meta-fixture-conformance

`rsi-meta-fixture-conformance` is the product-wide black-box conformance runner and schema-contract suite. It assembles maintained native packages and fixtures into release evidence for the public composition platform.

## Evidence boundary

The suite drives `CompositionHost`, C ABI tables, external envelopes, schemas, process-owned files, failpoints, and real dynamic libraries. It does not inspect private registry, routing, store, or loader implementation types beyond using the public loader package to validate artifacts. The product [testing guide](../../../crates/rsi-meta/docs/testing.md) defines how that evidence supports change and release claims.

## Running conformance

Run the repository xtask for manifest-scoped formatting, Clippy, tests, release builds, real-library loading, and the thirteen-step release demonstration across maintained standalone plugin and fixture workspaces:

```sh
cargo xtask rsi-meta conformance
```

The command supports Linux x86_64 and macOS arm64 and reports only the host it actually executes. The xtask owns process orchestration; this fixture continues to own the conformance catalog, assertions, and real-library runner.
