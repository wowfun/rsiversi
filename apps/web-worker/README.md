# rsi-web

This package adapts the [shared GUI application](../../crates/rsi/gui/README.md) to a Dedicated
Worker. It owns browser authentication, Worker exports, binary transfer and
API-backed renderer generation leases. Session controllers, projections,
submission reconciliation and GUI actions belong to the shared application.

One same-origin authenticated connection supplies ordinary domain clients.
Login consumes a device registration receipt and exchanges the token through the
HttpOnly cookie owner; browser storage retains no credential. Closing the
application drains its requests and surfaces without stopping the remote Host.
A WASM trap requires a fresh Worker and does not prove clean Rust shutdown.

The document requests one coalesced frame, mounts admitted renderers and then
acknowledges its exact frame ID. One pending frame and one acknowledgement wait
are retained; a 30-second stalled acknowledgement closes and drains the
connection. Domain observation cursors advance independently. Renderer offers
wake the existing frame producer instead of adding a second frame queue.

Image bytes are bounded before copying into WASM. Canonical response receive
leases remain owned until the document transfer is created. The document owns
bounded preview URLs and persistent editing under its [contract](../web/README.md).
Worker shutdown cancels asset observation and joins it before connection cleanup.
Production transport uses TLS/H2; loopback HTTP requires explicit development
opt-in at both ends.

Local Web bootstrap exchanges a bounded launch ticket through the browser API
adapter before composing the ordinary cookie-authenticated Worker Profile. A
definitive rejection preserves its category in the user-facing diagnostic.
Only an uncertain exchange attempts a cookie recovery read, never another redemption.
Capacity, retired service, invalid response and uncertain outcome have distinct
bounded messages that contain no backend text or credentials.
The loopback bootstrap path supplies the HTTP opt-in without a separate checkbox.
