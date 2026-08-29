Read the family [contract](README.md) before changing Host composition behavior.

- Keep `rsi-host` generic: it may depend on `rsi-meta`, `rsi-meta-profile`, and foundation
  contracts, but never on standard product implementations.
- Test catalog and bootstrap behavior through `HostBuilder` and the resulting
  Host, not through private resolver tables.
- Building freezes all explicit inputs; do not add ambient discovery,
  linker-inventory registration, or mutable root Runtime access.
