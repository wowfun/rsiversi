# rsi-ai-auth

This package owns standalone credential resolution and secret redaction.
`CredentialManager` captures mutable sources before registry construction and
resolves explicit, in-memory, persistent-store, then captured-environment
values. `SecretValue` never reveals itself through Debug or Display. Plugin
credentials are supplied separately by `rsi-meta` secret configuration.
`CredentialStore` is synchronous because native keyring APIs are synchronous;
the standalone `rsi-ai` façade always resolves it on a blocking worker before
continuing async Prepare.
