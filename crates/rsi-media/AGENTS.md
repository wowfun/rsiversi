Read the family [contract](README.md) before changing Media behavior.

- Media identity is the SHA-256 of final canonical bytes, never a source URL,
  filename, or caller declaration.
- Decode and bound untrusted raster input before durable publication. Backends
  verify immutable objects again on read.
- Default tests use generated tiny images and temporary or memory stores.
