Read the family [contract](README.md) before changing storage behavior.

- Keep this family limited to non-session domain state. Agent facts, Settings,
  Credentials, and Media retain their own durability contracts.
- Backends own validation and durable publication for their medium; the domain
  layer serializes one domain and updates its in-memory view only after a
  backend write succeeds.
- Default tests use temporary paths and never inspect or modify real user data.
