Read the family [contract](README.md) before changing tool behavior.

- Registration, schema projection, and execution must use the same exact-name
  authority snapshot.
- Never hold a registry lock while invoking or awaiting tool code.
- Cancellation is cooperative and quiescent: a cancelled or timed-out call
  does not return until the tool future settles.
