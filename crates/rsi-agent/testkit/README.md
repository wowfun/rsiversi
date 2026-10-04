# rsi-agent-testkit

Deterministic process-local Agent fixtures. The Memory Store implements the
same mechanical contract as SQLite: append admission preserves the indexed
turn lifecycle, and Fact pages stop before the aggregate encoded-byte bound.
Checkpoint reads share their immutable backing allocation and retain each
caller's exact byte admission through the last returned clone or slice.
It supports explicit append-failure injection. Its ordinary factory is for
lifecycle and composition tests only.
Its validation boundary checks Session existence: every stored value is already
typed and every mutation preserves the mechanical invariants under the same
lock. It has no raw-file corruption or disk-reopen boundary to revalidate.

The reusable mechanical conformance harness runs unchanged against Memory and
SQLite. Backend-specific corruption, filesystem, reopen, and writer-lease
evidence remains with SQLite.
