# rsi-desktop-bridge

One bounded JSON request envelope contains its generated protocol descriptor and
one explicit operation. This protocol has no Session execution, arbitrary
filesystem path, generic IPC forwarding, clipboard read or process capability.
The native owner checks Tauri's remote Origin capability and the current main
window/root document. Mandatory HTTP sandbox CSP gives preview frames an opaque
Origin and prevents them from accessing the main document or native IPC before decoding a request of at most 512 KiB.

Export open accepts the existing Session export Start event. Write accepts one
existing event; finish declares transport EOF after verified Complete. Native
tokens are opaque and scoped to a single active save operation. The owner
retains the task until persistence or cancellation finishes. Close acknowledgments
carry a monotonically increasing attempt number, so a stale document response
cannot close a later document.

Web builds derive the expected descriptor from the reviewed generated snapshot.
The native executable derives it from source at compile time. A mismatch fails
before any native side effect.
