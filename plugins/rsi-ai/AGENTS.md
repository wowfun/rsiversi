- These trusted native wrappers expose [`rsi-ai`](../../crates/rsi-ai/README.md)
  adapters through `rsi-meta`. Each manifest must statically advertise only
  capabilities the configured package always provides.
- Configuration fixes endpoint, protocol, and secret source for one plugin
  instance. Keep credentials in `x-rsi-meta-secret` fields and out of Debug,
  snapshots, events, and tests.
- This subtree is one standalone Cargo workspace and lockfile. Default tests
  must use local transports and no live credentials.
