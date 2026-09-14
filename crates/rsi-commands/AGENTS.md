Read the family [contract](README.md) before changing command behavior.

- Commands are explicit API calls; never parse slash-prefixed user text in this
  family. Application shortcut grammars belong to their owning products.
- Registration is exact-name and lease-owned. Never await while holding the
  registry lock.
