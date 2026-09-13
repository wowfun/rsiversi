# rsi-agent-context

The single deep module for prompt projection, incremental model context, and
deterministic compaction. It consumes validated session Facts and emits bounded
provider-neutral Language messages. It never reads a Workspace implicitly and
never stores a second transcript.

Semantic compaction is a pure planning and replay operation over model-purpose
Facts. The selected builder's complete identity, Header, source Turn/Fact spans
and digests, prior summary coverage, Usage horizon and canonical view digest
bind a frozen plan. The executor supplies provider events; only natural Stop
with nonempty visible text, no Tool calls, at most 32 KiB of text and a strictly
smaller canonical view installs the summary. Reasoning is not summary content.
Invalid or mismatched summaries are inert, including summaries whose transitive
source is outside the exact fork selection. Raw Facts remain authoritative.

Pressure uses the most recent finished Conversation's reported input tokens
for the same ModelRef, at eighty percent of described context window minus
default output reserve. Summary Usage and Usage covered by an installed summary
are ineligible. Installing an eligible summary consumes the preceding Usage,
including inherited parent Usage; only new Conversation Usage can rearm it.
An optional Usage trigger with no selectable whole unit leaves the ordinary
request unchanged. Forced or canonical pressure without a selection is a limit.
No estimate substitutes for missing Usage. Hard canonical
byte/message limits and explicit provider ContextLimit handle first calls.
Compaction preserves active instructions, the current original task input, latest
human steering and whole recent interaction units. The optional recent tail
uses at most 64 KiB and half the configured message allowance (at most 512
messages); the last unit is always retained intact. A terminal Turn's incomplete
Tool batch is protected without fabricating missing results; unrelated complete
units remain eligible. Orphan results and unfinished live batches are invalid.
A capacity retry
halves selected canonical message bytes, choosing whole units oldest-first and
skipping any unit that cannot fit the remaining allowance.
An instruction replacement supersedes earlier instructions from that same
source, including tombstones; a complete skill catalog supersedes its preceding
catalog. Those historical versions become ordinary summary candidates. Additive
instructions stay protected until an explicit replacement of their source;
instructions from other sources retain their protection.

Builder 2.4.0 binds newly selected source spans plus the exact previously
installed summary. That prior is an inductive proof: it is usable only after
its own sources and prior were validated in this same replay/fork selection.
Transitive raw bindings are not copied into every descendant plan. Fully
summarized completed Turns and their source metadata are released. The cold
materialization bounds count projected messages (4,096 / 32 MiB), not Facts;
raw source metadata shares the 4,096 cold retention ceiling. At 1,024 retained
sources, every subsequent planning opportunity requests compaction. Each plan
selects at most 1,024 source Turns, oldest first. Selection stops before the
encoded sources and selections exceed 240 KiB, reserving 16 KiB within the
protocol's 256 KiB plan bound for bounded identity, horizon and summary metadata.
This preserves whole interaction units even with maximum-length identifiers;
the same rule governs replay eligibility and the smaller retry.
Selection also counts the JSON-quoted source bytes, prior summary and complete
no-Tool request envelope against the AI protocol's request-byte bound. Units
that cannot fit the remaining request allowance are skipped intact. A single
unsplittable unit above that allowance stays raw; forced pressure can still fail.
Skipping that first opportunity
does not invalidate later replay within the cold bounds; raw history is never
silently evicted to make room.

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
sampling. The ordinary `DefaultContextBuilderFactory` provides semantic
compaction over ContextFold and accepts only null configuration; selecting it is an
explicit Agent Profile choice.

`ModelContextState` owns the selected builder, cursor, and version-6 cache
envelope. The envelope binds the builder ID, semantic version and normalized
configuration digest, Header fingerprint, exact limits, cursor and Fact prefix
to the raw bounded provider payload. Restore validates this envelope before
calling the builder and requires the restored cursor's position to agree.
Rejected or mismatched caches are rebuilt from Facts; a failed restore leaves
the current cursor intact. The Store's single Session cache slot does not change. The envelope writes metadata and raw payload once without
deep-cloning projected messages. The default provider uses the version-7 fold
encoding below as its opaque payload; other providers own their payload schema.

Exact Fact prefixes with no active model assembler may be encoded as the
version-7 Context checkpoint. Retained nonterminal turns are encoded with their
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
