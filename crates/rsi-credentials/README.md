# rsi-credentials

`rsi-credentials` owns secret resolution independently from Settings and AI.
The [`rsi-credentials-protocol`](protocol/README.md) package defines stable
`(owner PluginId, slot)` references and redacted secret values.
[`rsi-credentials-local`](local/README.md) is an ordinary plugin backed by the
private local credential file with an explicitly captured startup environment fallback.
[`rsi-credentials-testkit`](testkit/README.md) provides deterministic memory
behavior.

Consumers resolve once per external operation and retain no cross-operation
cache. A saved entry wins over the captured environment. Only an absent file or
entry permits environment fallback; storage failures never select another secret.
Administrative writes may replace an environment-provided credential. Configuration contains
only references and environment-variable names, never secret values. Resolve
and Admin are separate Local contracts; an implementation never infers or
shares values across different owner identities.

The [local provider contract](local/README.md) owns file format, permissions,
publication and failure semantics. Historical keyring provenance remains readable
in durable call facts; no current backend reads, imports or deletes keyring data.
