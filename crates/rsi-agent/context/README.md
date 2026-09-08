# rsi-agent-context

The single deep module for prompt projection, incremental model context, and
deterministic compaction. It consumes validated session Facts and emits bounded
provider-neutral Language messages. It never reads a Workspace implicitly and
never stores a second transcript.

Entered plugin context is durable text with developer role. A pre-start Tool
rejection becomes an error response for its exact model call, without inventing
Tool execution or consulting a current plugin during replay.

`ModelContextBuilder` is a synchronous, process-local Local capability. It opens
one mutable `ModelContextCursor` from a validated immutable Header, retention
limits, and an optional bounded provider checkpoint payload. Cursors consume
framework-supplied canonical pages, claim-visible pages with their scan horizon,
fork seed pages, and explicit seed completion as distinct inputs. They build
provider-neutral requests from the Tool definitions supplied by the same Agent
composition pin. Builders and cursors perform no external I/O or implicit clock
sampling. The ordinary `DefaultContextBuilderFactory` provides the existing
ContextFold behavior and accepts only null configuration; selecting it is an
explicit Agent Profile choice.

`ModelContextState` owns the selected builder, cursor, and version-6 cache
envelope. The envelope binds the builder ID, semantic version and normalized
configuration digest, Header fingerprint, exact limits, cursor and Fact prefix
to the raw bounded provider payload. Restore validates this envelope before
calling the builder and requires the restored cursor's position to agree.
Rejected or mismatched caches are rebuilt from Facts; a failed restore leaves
the current cursor intact. The Store's single Session cache slot and durable
schema do not change. The envelope writes metadata and raw payload once without
deep-cloning projected messages. The default provider uses the version-5 fold
encoding below as its opaque payload; other providers own their payload schema.

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
