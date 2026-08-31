# rsi-media-protocol

This package owns immutable image references and the split Media service/backend
contracts. It contains no codec implementation, filesystem, HTTP fetch,
provider projection, or plugin lifecycle.

`StoredMedia` intentionally redacts byte contents from Debug output. Backends
must verify that bytes match the reference identity before returning them.
Descriptor MIME types are canonical lowercase `image/*` or `audio/*` values so
provider resolution never depends on case-folding a durable identity.
Descriptors describe validated external/provider inputs and may name supported
source formats. A durable `MediaRef` instead names canonical PNG bytes produced
by the Media service and enforces both the pixel and per-dimension bounds;
callers must not treat the two roles as interchangeable.

The closed error taxonomy distinguishes permanent malformed or out-of-bounds
input from transient generation admission pressure. Callers may retry
`AdmissionFull`; they must not reinterpret `InvalidInput` as backpressure.
