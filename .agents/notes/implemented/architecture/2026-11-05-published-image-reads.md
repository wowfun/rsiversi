---
name: Read normalized images from their publisher
---

## Problem

The original DSH attachment store hashes the final encoded raster. Its
normalizer can retain a clean JPEG or WebP byte for byte. Resubmitting that
raster to the native PNG importer changes its digest and makes each upload
occupy two stores. Passing the encoded input through application IPC also
charges the transport for bytes already on the same machine.

## Decision

The mounted attachment reader uses the publisher's retained directory for both
verbatim files and normalized images. The native Media service consults this
reader when its ordinary backend reports a missing object. Both Session input
and the provider obtain the same verified bytes by digest. A per-message display
name and pre-resize dimensions belong to the occurrence. Two filenames for the
same pixels can therefore coexist in replay.

## Alternatives considered

A single writable store would require both publishers to adopt the same
envelope, atomic writes and retention policy. The two existing publishers
already produce stable, verifiable content identities independently.

## Consequences

Lookup pays one primary miss for a publisher-only image. Keeping the namespaces
independent lets ordinary native writes retain their atomic envelope and lets
the original uploader own its receipt and publication lifetime. A digest, header
or I/O failure in the primary store surfaces to the caller.

The storage layout and input bounds belong to the [local reader](../../../../crates/rsi-media/local/README.md).
The durable occurrence belongs to the [Session protocol](../../../../crates/rsi-agent/session-protocol/README.md).
