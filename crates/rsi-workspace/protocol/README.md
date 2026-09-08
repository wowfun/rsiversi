# rsi-workspace-protocol

This library owns validated Workspace identities, exact registration reads and
bounded insertion-order pages. It contains no native filesystem or storage
implementation. A returned path names a directory on the registry's host; it is
not authority to open a directory on the caller's device. Unknown identities
are typed errors. Listing cursors remain usable after deleting a registration.

Workspace paths are UTF-8 and bounded to 16 KiB before registration or durable
loading. Adapter responses validate record identity/path bounds, unique page
identities, requested count and advancing continuation before retention. A path
in a remote response remains opaque host data; client-native path syntax does
not validate another host's filesystem.

Remote proxies preserve API failures in `WorkspaceError::Api`, including unknown
mutation outcomes. A failed reply is not proof that registration or deletion did
not happen. The caller reconciles through exact registry identity and reads; the
proxy does not replay mutations or reinterpret API failures as storage failures.
