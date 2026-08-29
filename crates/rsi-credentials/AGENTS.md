Read the family [contract](README.md) before changing credential behavior.

- Secret bytes are never exposed by `Debug` or `Display`; secret wrappers may
  implement redacted `Debug`, but never serialization or equality.
- Provider consumers receive only the resolve contract; mutation requires the
  separate admin contract.
- Default tests inject memory stores and captured environment snapshots. Never
  touch the developer's OS keyring or ambient environment.
