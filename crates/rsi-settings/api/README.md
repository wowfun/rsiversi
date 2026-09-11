# rsi-settings-api

The [Settings contract](../protocol/README.md) owns the shared domain types and semantics.

Ordinary endpoint and client plugins expose the registered-namespace
`SettingsAccess` contract through versioned list, describe, read, replace and clear operations.
They do not expose registration, validators, raw documents or unregistered
sections. Namespaces and section values retain their protocol-owned bounds.
Finite requests and snapshots use at most 8 MiB including envelope metadata;
the actual section remains limited to 4 MiB. All operations use the Data lane.

Replace and clear carry both registration identity and revision. Successful
responses must retain that identity and advance the revision exactly once.
Retired scope identities and stale revisions remain distinct domain failures.
Malformed mutation responses and native I/O/commit-task failures preserve unknown
outcomes; clients never replay writes. Connection failures retain their API
classification. No secret or raw-document access is introduced by this interface.

List and describe require authenticated access. Clients validate page count,
lexical order, cursor progress, exact namespace binding, metadata and default
value bounds before exposing descriptions. Discovery carries no authority to
register namespaces or to bypass the existing write scope and revision checks.

Replace and clear also require an explicitly composed SettingsMutationPolicy.
The policy receives the trusted connection origin, exact namespace and proposed
replacement (or clear), and returns a retained admission lease. The endpoint
holds that lease through the complete domain mutation, including durable commit.
This permits an application-owned grant to close admission and drain existing
writes before acknowledging revocation. The policy cannot be selected or bypassed
by a request field. LocalSettingsPolicy is the explicit Local-only policy for
standalone composition. Product-specific namespace rules remain with the product.

Tests exercise namespace isolation, validation, last-good state, exact CAS and
re-registration through the public API with isolated Settings providers.
