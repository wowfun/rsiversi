# rsi-session-testkit

This library owns shared behavioral assertions for Session adapters. A caller
supplies an isolated real service, an unused creation request, its expected
canonical workspace path, and explicit Execution. The service's fixture model
must complete a text request successfully without live credentials.

The same scenario checks draft history, idempotent mailbox acceptance, immutable
message reads, conflicting input, durable claim and completion, attachment,
recent listing and durable creation collisions. It never constructs a native
backend, opens a file, captures an ambient scheduler, or owns provider setup.
Adapter tests own transport-specific malformed input and disconnect assertions.
