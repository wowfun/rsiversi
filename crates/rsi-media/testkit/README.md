# rsi-media-testkit

This package provides an ordinary in-memory immutable Media backend for
deterministic codec and provider tests. It verifies idempotent identity and
the byte length and SHA-256 identity at its backend boundary. It does not touch
the filesystem, network, or user media.
