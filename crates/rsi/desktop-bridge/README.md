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

Web and native builds read the owner's explicit wire-contract.json declaration.
Before window creation, Desktop checks the descriptor retained with the actual
Web bundle or reported by its supervised frontend. Each call also checks it.
The shared conformance corpus exercises JavaScript encoding and Rust decoding.
