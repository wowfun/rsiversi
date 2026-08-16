# Echo composition

This development composition mounts the real bidirectional echo fixture in a
root scope and binds a nested-scope consumer to it by stable instance id.

It demonstrates one successful local path. See [configuration](../../../crates/rsi-meta/docs/subsystems/configuration.md), [protocol](../../../crates/rsi-meta/docs/subsystems/protocols.md), and [security](../../../crates/rsi-meta/docs/security.md) for the full contracts.

Build the two native packages for the current supported target, then create the
lock, install it into a state directory, and start the foreground daemon:

```sh
host_target="$(rustc -vV | awk '/^host:/ { print $2 }')"
cargo build --locked --release --target "${host_target}" \
  --manifest-path fixtures/rsi-meta/echo-bidi/Cargo.toml
cargo build --locked --release --target "${host_target}" \
  --manifest-path fixtures/rsi-meta/nested-scope-consumer/Cargo.toml
cargo run --locked -p rsi-meta-cli --bin rsi-meta -- \
  lock examples/rsi-meta/echo/rsi-meta.toml --lock examples/rsi-meta/echo/rsi-meta.lock
cargo run --locked -p rsi-meta-cli --bin rsi-meta -- \
  --state-dir ./.rsi-meta install examples/rsi-meta/echo/rsi-meta.toml \
  --lock examples/rsi-meta/echo/rsi-meta.lock
cargo run --locked -p rsi-meta-cli --bin rsi-meta -- \
  --state-dir ./.rsi-meta daemon serve
```

The generated `rsi-meta.lock` is local target state and is intentionally not
checked in.
