# rsi-credentials

`rsi-credentials` owns secret resolution independently from Settings and AI.
The [`rsi-credentials-protocol`](protocol/README.md) package defines stable
`(owner PluginId, slot)` references and redacted secret values.
[`rsi-credentials-local`](local/README.md) is an ordinary plugin backed by the
OS keyring with an explicitly captured startup environment fallback.
[`rsi-credentials-testkit`](testkit/README.md) provides deterministic memory
behavior.

Consumers resolve once per external operation and retain no cross-operation
cache. The keyring wins over the captured environment. Configuration contains
only references and environment-variable names, never secret values. Resolve
and Admin are separate Local contracts; an implementation never infers or
shares values across different owner identities.
