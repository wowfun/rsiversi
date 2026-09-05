# rsi-agent-context

The single deep module for prompt projection, incremental model context, and
deterministic compaction. It consumes validated session Facts and emits bounded
provider-neutral Language messages. It never reads a Workspace implicitly and
never stores a second transcript.

Exact Fact prefixes with no active model assembler may be encoded as the
version-5 Context checkpoint. Retained nonterminal turns are encoded with their
lifecycle state, so accepted queued turns do not prevent a checkpoint. Context
alone owns and validates that schema, recomputes all message accounting on
restore, and binds the retained projection to the immutable header, exact
retention limits, cursor, and a rolling SHA-256 digest of every folded Fact.
The envelope serializes the borrowed retained projection once, then prefixes
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

An accepted mailbox turn becomes checkpointable only after its first
model-visible input has entered; Context never writes an empty turn that its
restore boundary would reject.

Provider replay evidence is durable transcript evidence, not a portable prompt
token. Context therefore builds a provider-neutral request without consulting a
generic capability profile: the current AI seam exposes endpoint, configuration
generation, and credential source only after preparation. Until those exact
route facts can be preflighted together, Context never uses replay evidence to
elide canonical history and removes
provider-private reasoning blocks from the next provider request. Visible text,
tool calls, tool results, and the complete retained turn prefix remain. This is
the fail-closed boundary that prevents one deployment's response identity from
crossing into another deployment that accepts the same extension format.

A fork fold owns the complete inherited interval recorded in the child Header.
Seed pages must begin immediately after `resolved_after_seq`, remain contiguous
across page boundaries, and finish exactly at `resolved_terminal_seq` before
child Facts may be projected. The inherited interval must also contain only
balanced completed turns at that boundary. The parent interval does not advance
the child's Fact cursor or Fact-prefix digest.
