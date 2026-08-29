Read the family [contract](README.md) before changing projection behavior.

- Projection output is derived and disposable; never use it as a fact or write
  authority.
- Registrations are exact-name and lease-owned. Never hold the registry lock
  while running projection code.
