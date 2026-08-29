# rsi-settings

`rsi-settings` owns user-editable, non-secret configuration. The
[`rsi-settings-protocol`](protocol/README.md) package defines provider and
consumer seams. The ordinary [`rsi-settings`](core/README.md) plugin owns
namespace registration and resolves `defaults -> composition base -> user
section`. [`rsi-settings-local`](local/README.md) provides one explicit JSON
document, while [`rsi-settings-testkit`](testkit/README.md) supplies a memory
provider for deterministic tests.

Namespace registration is caller-owned and duplicate names fail loud. Every
write uses a namespace revision CAS. A provider persists before the service
publishes the new value. Settings never stores credentials or guesses a
provider-specific merge policy.
