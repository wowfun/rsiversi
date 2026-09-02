Read the family [contract](README.md) before changing command behavior.

- Commands are explicit API calls; never parse slash-prefixed user text in this
  family or in a Session application.
- Registration is exact-name and lease-owned. Never await while holding the
  registry lock.
