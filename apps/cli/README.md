# rsi-cli

The native RSI launcher owns command parsing, process control and Tokio setup. It
uses the explicit application catalog for native clients and daemon startup.
`cargo run --locked -p rsi-cli -- --help` does not activate a backend.
Product Web startup requires a paired distribution from `pnpm -C apps/web build`;
a plain Cargo executable remains usable for applications without Web assets.

Product Web integration tests are opt-in: build a complete paired publication,
then run `RSI_PAIRED_BUNDLE="$PWD/target/rsi-app/current" cargo test --locked -p
rsi-cli --features paired-web-tests --test service_host_cli web::`. The directory
must contain its paired `rsi`, `assets/` and receipt. Missing inputs fail explicitly;
these tests never fall back to an ordinary Cargo executable and arbitrary assets.
