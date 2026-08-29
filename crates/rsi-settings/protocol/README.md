# rsi-settings-protocol

This package owns the runtime-independent Settings provider and consumer
contracts. Values are bounded JSON; schema validation is a caller-supplied
safe-Rust function and namespace revisions protect writes from stale clients.

The package contains no files, environment access, plugin lifecycle, or global
registry.

Owning tests cover namespace syntax and bounds plus exact encoded-section byte
admission; provider and registry suites cover the stateful uses of those pure
validators.
