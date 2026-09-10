# rsi-media-protocol

This package owns immutable image references and the split Media service/backend
contracts. It contains no codec implementation, filesystem, HTTP fetch,
provider projection, or plugin lifecycle.

`StoredMedia` intentionally redacts byte contents from Debug output. Backends
must verify that bytes match the reference identity before returning them.
Source images and returned bodies use immutable shared `bytes::Bytes`. Clones
and slices preserve the original allocation owner, including an API receive
lease. Moving a body through a service or provider must not flatten that owner
into an uncharged copy. `MediaError::Api` retains common transport failures.
Descriptor MIME types are canonical lowercase `image/*` or `audio/*` values so
provider resolution never depends on case-folding a durable identity.
Descriptors describe validated external/provider inputs and may name supported
source formats. A durable `MediaRef` instead names canonical PNG bytes produced
by the Media service and enforces both the pixel and per-dimension bounds;
callers must not treat the two roles as interchangeable.

The closed error taxonomy distinguishes permanent malformed or out-of-bounds
input from transient generation admission pressure. Callers may retry
`AdmissionFull`; they must not reinterpret `InvalidInput` as backpressure.

Image import is independently durable: success returns the canonical `MediaRef`
after publication. Clients may submit that reference in a later Session Message
or reuse it across Sessions. A failed or cancelled later operation does not
roll back the upload. Losing an import reply may leave a published object;
retrying the same source is safe under content-addressed publication. No caller
may delete an object as compensation for Message failure. Cross-service staging,
reference tracking and garbage collection are separate work.
