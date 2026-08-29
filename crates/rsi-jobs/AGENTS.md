Read the family [contract](README.md) before changing Jobs behavior.

- Jobs are process-local and must never be presented as crash-recoverable.
- Cancellation is cooperative; cleanup cancels and joins generation-owned work
  under an explicit bound.
- Keep Agent durable scheduling and external provider jobs out of this family.
