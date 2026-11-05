# rsi-media-protocol

This package owns immutable image/file references and the split Media service/backend
contracts. It contains no codec implementation, filesystem, HTTP fetch,
provider projection, or plugin lifecycle.

`FileRef` describes a verbatim file independently of raster media. Its private
fields and deserializer enforce a SHA-256 identity, a portable UTF-8 leaf name of
at most 255 bytes, and a byte length representable exactly by JavaScript (zero
is valid). A path, URL, upload receipt, or media type is not part of the reference.
`FileRead` separates streaming integrity verification from local path resolution;
the latter is a location, not an access grant. Implementations may share a
publisher's immutable namespace without copying objects into another store.

`StoredMedia` intentionally redacts byte contents from Debug output. Backends
must verify that bytes match the reference identity before returning them.
Source images and returned bodies use immutable shared `bytes::Bytes`. Clones
and slices preserve the original allocation owner, including an API receive
lease. Moving a body through a service or provider must not flatten that owner
into an uncharged copy. `MediaError::Api` retains common transport failures.
Descriptor MIME types are canonical lowercase `image/*` or `audio/*` values so
provider resolution never depends on case-folding a durable identity.
Descriptors describe validated external/provider inputs and may name supported
source formats. A durable `MediaRef` names a published PNG, JPEG, WebP or GIF
by the digest of its final encoded bytes, length, MIME type and dimensions.
`MediaObjectReader` verifies objects by identity. An optional
`PublishedImagesContract` supplies objects owned by a separately mounted
publisher. The Media service checks the caller's metadata against the object.

The closed error taxonomy distinguishes permanent malformed or out-of-bounds
input from transient generation admission pressure. Callers may retry
`AdmissionFull`; they must not reinterpret `InvalidInput` as backpressure.
`Unsupported` identifies a missing backend or platform capability before
admission. The Media API preserves that category without transmitting backend
diagnostics.

Image import is independently durable: success returns the canonical `MediaRef`
after publication. Clients may submit that reference in a later Session Message
or reuse it across Sessions. A failed or cancelled later operation does not
roll back the upload. Losing an import reply may leave a published object;
retrying the same source is safe under content-addressed publication. No caller
may delete an object as compensation for Message failure. Cross-service staging,
reference tracking and garbage collection are separate work.

Image presentation metadata accompanies an input occurrence. Its optional display name is a leaf of up to 255 UTF-16 code units. Optional original dimensions describe positive source pixel dimensions before resizing. Media identity and AI descriptors use the published raster only.
