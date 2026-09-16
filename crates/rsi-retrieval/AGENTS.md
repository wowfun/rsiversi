Read the family [contract](README.md) before changing retrieval behavior.

- Keep model-chosen URL policy here; operator-configured MCP and AI endpoints
  have different trust boundaries and must not inherit this policy.
- Check complete DNS answers and pin them to the connection. Never use an
  ambient proxy or silently fall back after a policy failure.
- Bound wire bytes before decoding and parsing. Preserve explicit truncation.
- Keep results external data, Credentials separate, and history free of network
  activity. Generic Agent code owns neither HTTP nor Exa semantics.
- Tests are deterministic by default. Live network access is opt-in.
