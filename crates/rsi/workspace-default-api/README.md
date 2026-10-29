# rsi-workspace-default-api

The standard product initializes its first workspace through one authenticated,
idempotent mutation. It accepts no path or model. The Service uses its captured
existing Documents directory and creates only its `RSIversi/default-workspace`
children through retained no-follow directory handles, independently of
Session creation and provider configuration. An existing workspace or persisted
Session prevents first-use creation. A recorded initialization survives removal
of its registration; it never recreates a deliberately removed workspace.

The response contains the current registration when available. The ordinary
client validates its coordinates and identity before exposing it to GUI code.
The Service owner keeps one bounded initialization receipt in its own domain,
retains admitted work through settlement, and never deletes directory contents.
Directory failure leaves initialization retryable. If registration succeeded
before its receipt failed, retry resolves the native canonical directory identity to recognize that registration and completes
the receipt instead of creating another directory or workspace.

Read and directory failures are backend diagnostics, bounded to 512 UTF-8 bytes
at the client boundary; they are not malformed requests or uncertain mutations.
`OutcomeUnknown` is reserved for interrupted admitted work and an uncertain
receipt write. Retrying always rechecks registry and history before registration.
