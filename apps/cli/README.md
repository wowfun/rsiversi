# rsi-cli

The native RSI launcher owns command parsing, process control and Tokio setup. It
uses the explicit application catalog for native clients and daemon startup.
`cargo run --locked -p rsi-cli -- --help` does not activate a backend.
Product Web startup requires a paired distribution from `pnpm -C apps/web build`;
a plain Cargo executable remains usable for applications without Web assets.

For a manual native build, run from the repository root:

```bash
cargo build --locked -p rsi-cli
target/debug/rsi --help
```

The native executable includes all of its linked backends. Application Profile
selection changes runtime composition, not the executable's Cargo dependency
graph. The [development guide](../../docs/development.md) starts the standard
debug product with caller-selected state; [source installation](../../docs/user/source-install.md) owns
managed native, Web and Desktop build targets and launch selection.

Product Web integration tests are opt-in: build a complete paired publication,
then run `RSI_PAIRED_BUNDLE="$(bash scripts/rsiversi.sh path web)" cargo test --locked -p
rsi-cli --features paired-web-tests --test service_host_cli web::`. The directory
must contain its paired `rsi`, `assets/` and receipt. Missing inputs fail explicitly;
these tests never fall back to an ordinary Cargo executable and arbitrary assets.

Headless process tests synchronize the phase they exercise before sending a
signal. Windows ACP integration exercises the built native binary with isolated
XDG paths and a loopback model fixture, including model/effort selection, cold
resume and stdio EOF. Linux MCP fixtures remain Linux-only. Tests of unavailable
capabilities continue to assert refusal before effects.
Provider entry establishes execution readiness; the cancellation-output
test also waits for the complete JSONL Turn record. These bounded waits retain
captured output on failure and reap the child. The
[terminal contract](../terminal/README.md) owns interruption and output deadlines.

The debug executable reserves internal `__dev` actions for supervised source
development. Their bounded child-pipe contract is owned by
[development control](../dev-control/README.md). They reuse ordinary Profile,
path and provider resolution; ordinary Web grammar exposes no Origin override.
The internal browser opener reuses the normal OS handoff without starting a Host.
