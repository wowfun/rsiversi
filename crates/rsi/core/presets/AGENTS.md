- This namespace owns product-shipped Agent-preset source assets. Keep each
  preset in a validated id directory with required `agent.profile.toml` and
  optional `preset.toml`; do not add runtime state or user-authored content.
- Treat asset bytes as composition inputs. Update the owning `rsi` materializer
  and its byte-verification tests with every asset or layout change.
- These files are repository data, not a standalone plugin workspace. Do not
  add a Cargo manifest, lockfile, generated cache, or platform-specific copy.
