# rsi-agent-context

The single deep module for prompt projection, incremental model context, and
deterministic compaction. It consumes validated session Facts and emits bounded
provider-neutral Language messages. It never reads a Workspace implicitly and
never stores a second transcript.

Exact Fact prefixes with no active model assembler may be encoded as
`ContextCheckpointV3`. Retained nonterminal turns are encoded with their
lifecycle state, so accepted queued turns do not prevent a checkpoint. Context
alone owns and validates that schema, recomputes all message accounting on
restore, and binds the retained projection to the immutable header, exact
retention limits, cursor, and a rolling SHA-256 digest of every folded Fact.
The V3 envelope serializes the borrowed retained projection once, then prefixes
its raw digest; checkpoint creation neither deep-clones the retained messages
nor serializes the payload twice. Fact-prefix hashing streams canonical JSON
directly into SHA-256, and the immutable system message plus its canonical byte
size are cached once per fold.
The Store carries that prefix digest independently so the executor can reject
bytes that no longer describe the canonical prefix. A claim-filtered sequence
hole, active assembler, wrong header, wrong limits, changed payload, or
malformed bytes makes the fold non-checkpointable. The checkpoint is an
integrity-checked cache written by the trusted in-process Context owner, not an
authentication boundary against coordinated replacement of both Store metadata
and cache bytes.
