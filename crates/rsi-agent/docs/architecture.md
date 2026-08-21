# rsi-agent architecture

## Product interface

[`AgentHost`](../core/README.md) is the only runtime façade. Opening it acquires the agent workspace, opens the strict schema-v7 transcript and AI-operation store, repairs incomplete sessions and operations, validates the artifact CAS, and starts private coordination and storage machinery. A caller supplies an existing `CompositionHost` and a consumer instance authorized by its graph; the agent host retains a clone of that composition handle and never owns composition shutdown.

`AgentHost` is cheap to clone, and `run` and `transcript` borrow it immutably. `run` accepts one caller-chosen `SessionId`, exact model, and prompt. The session has exactly one turn in v0. A terminal session with the same original model and prompt is an idempotent lookup and performs no service work. Concurrent calls for the same identifier, model, and prompt join one admitted session task; changing either request field is rejected whether the first run is active or terminal. `transcript` waits for that session when it is active, without waiting for unrelated work, then returns immutable typed events and the derived terminal outcome.

`OpenOptions::with_max_concurrent_runs` accepts a `NonZeroU8` active-session limit; the default is eight. The host accepts at most four times that many `run` calls, including same-session joiners; further futures wait for admission without creating a session task or coordinator message. Unrelated admitted sessions use separate tasks. A turn still has one ordered state machine and invokes its tools strictly serially.

`OpenOptions::with_execution_limits` installs one host-wide `ExecutionLimits` value so callers joining a durable session cannot disagree about deadlines. Its constructor requires nonzero handshake, model-response, tool-response, and provider-turn durations, and each operation duration must not exceed the provider-turn duration. Defaults are 10 seconds, 60 seconds, 30 seconds, and 5 minutes. Deadline arithmetic is checked again when a run starts, so an extreme duration accepted by a long-lived host closes that run as timed out instead of panicking after the monotonic clock advances. The provider-turn clock bounds provider operations from stream opening through stream finish; it is not a cancellation deadline for durable terminal closure, which may continue after a timeout to close the transcript reliably.

Dropping a `run` future does not cancel accepted work. Its session task continues until it commits a terminal outcome or the shared host enters its fail-closed terminal state. Dropping one host clone does not affect the others. When the last façade is dropped, admission closes, already accepted work drains, and the workspace lease remains held until the private workers exit. v0 exposes no cancellation or explicit shutdown operation.

## Coordination and storage

A small coordinator owns admission, the active-session registry, same-session joiners, concurrency permits, and the shared terminal flag. It does not execute model calls, tool calls, or SQLite statements. An admitted session task owns one turn's service streams and state machine; a failure in one ordinary run closes that run without serializing unrelated sessions behind provider latency.

The first request for an inactive identifier creates a probing entry. Requests that arrive during the durable lookup are grouped by exact `(model, prompt)` rather than copied into the eventual running entry. If the session already exists, each group is compared with the validated durable context and prompt independently. If it is new, the first accepted pair owns the run and only that group's waiters move into the running state. Requests and run records share their immutable payloads internally, so joining or fan-out does not copy request or terminal data.

SQLite stays a concrete deep module. One dedicated blocking writer thread owns the mutable connection, serializes transactions, and returns compact commit receipts over a bounded channel. WAL uses `synchronous=FULL` deliberately: an acknowledged effect barrier must survive power loss, not only process termination, before external invocation begins. Cold replay and transcript lookup acquire a private semaphore capped at `min(max_concurrent_runs, 4)` and run as bounded blocking jobs over a lazy pool with the same cap; a healthy read-only connection is returned to that pool after its pathname and handle identity are rechecked. This removes a permanent reader thread and repeated open/schema-validation work while avoiding head-of-line blocking and an unbounded connection pool. No `rusqlite` call runs on a Tokio worker.

The writer and every accepted cold read hold clones of one `Arc`-owned workspace lease. Field and scope ordering close each SQLite connection before releasing its lease clone, and the last clone cannot disappear while accepted storage work remains. The lease is Unix-specific: the crate rejects non-Unix targets at compile time instead of substituting a process-local-only lock.

The session task batches adjacent log transitions whenever no external effect occurs between them. The initial boundaries commit together; context and the first request share a transaction; an assistant response and all prepared calls share one; a prior result may commit with the next dispatch marker; and the last results, continued-step boundaries, and next model request may commit together. Including session creation, a direct-final turn uses five transactions and the one-tool/two-model path uses nine. The safety barriers do not move: every dispatch marker is durable before invocation, every result is durable before a later tool invocation or model request can observe it, request bytes are rederived after their transaction commits, and terminal boundaries remain one atomic update.

`SessionTxn` is the private transition boundary. It takes the installed `SessionMachine`, applies one intent into an immutable pending commit, and withholds the candidate state while the writer performs a compare-and-set on `next_seq` and `payload_bytes`. Only a matching compact receipt installs the next machine state and exposes a committed request or dispatch. Request preparation performs exactly two projections: once while forming the pending event and once from the installed post-commit state before the adapter receives those same bytes. Live execution therefore neither rereads SQLite nor carries a second transcript-shaped state machine.

The coordinator retains active-session identity only while work or joiners exist; completed transcripts and outcomes are not cached. Every later replay or transcript lookup rereads and validates that session in a cold-read job, keeping durable rows authoritative and cache invalidation out of the runtime. Cold-read admission and the writer queue are bounded, so storage pressure propagates to session tasks and then callers rather than spawning blocking work without limit. A writer result whose durability cannot be confirmed atomically sets the shared terminal flag; active tasks stop at their next durability or external-effect barrier, while the affected session reports recovery-required identity when available.

## Turn execution

```text
session + turn + user message commit
                |
                v
open generation-pinned model and tool streams
                |
                v
validate catalog + commit context snapshot
                |
                v
derive, commit, install, rederive, and send model request
                |
          +-----+-----+
          |           |
     tool calls    final text
          |           |
prepare all calls     |
          |           |
dispatch + result     |
serially per call     |
          |           |
end step, next step   |
          +-----+-----+
                |
       close streams and commit terminal outcome
```

One model stream and one tool stream remain open for the turn, pinning both provider generations. Their initialization runs concurrently, and normal shutdown attempts both concurrently even if one fails. The context snapshot records both provider instances, their semantic protocol versions, the fixed system prompt, and the normalized tool catalog. The catalog and all invocations therefore use one tool provider. Tool calls within and across steps execute in model order; there is no parallel dispatch.

The core-owned fixed system prompt is `You may use the supplied tools. After observing tool results, return a final answer.` The protocol merely carries and bounds that provider-neutral string. A model step may return final nonempty text immediately or prepare calls. Application-level tool failures, including unknown tools and argument-schema violations, become model-visible tool results. Malformed service messages, duplicate call identifiers, transport failures, empty final output, and resource-limit violations close the turn as failed.

## Transcript and request derivation

The SQLite transcript is the source of truth. Events use session-local sequence numbers beginning at one without gaps and strictly nest one turn and its steps. The typed stream records lifecycle boundaries, the normalized context snapshot, request snapshots, assistant messages, prepared calls, dispatch starts, tool results, and the terminal outcome.

Every model request is derived exclusively from committed context, message, and tool-result events. Before sending, the runtime commits the request's source sequence, canonical bytes, and SHA-256 digest, derives it again from the committed prefix, and verifies byte equality. The adapter sends those same canonical bytes. Consequently every model-visible item is logged and every recorded request is independently reconstructable.

An assistant response and all calls it prepares commit atomically. Each call commits `ToolDispatchStarted` before provider invocation when dispatch occurs and has exactly one terminal result in a closed transcript. A final assistant message commits before stream closure; after both service streams close normally, the inner-to-outer completed step and turn boundaries commit atomically.

The transcript preserves each tool call's original argument text for audit and model-request reconstruction. Before dispatch, the runtime parses it with duplicate-key and complexity checks and converts only numbers whose decimal value is exactly representable by the finite `f64` machine values used by the JSON Schema validator. Lossy numbers become a model-visible invalid-arguments result without dispatch. The runtime validates the accepted value and sends the provider its canonical encoding, so schema validation and tool execution cannot observe different numeric values.

The store uses SQLite WAL mode, foreign keys, `synchronous=FULL`, an exclusive workspace lease, and strict schema version 7. The caller's workspace path is resolved once before product files are created; the writer and every cold read derive their database path from that canonical leased root. The `sessions` table contains the prompt, terminal bit, next sequence, and cumulative payload bytes; `ai_operations` contains a caller-owned operation identifier, an open operation's bounded prepared snapshot and phase, or a compact terminal tombstone with a monotonic completion order. Terminal session status and prompt provenance are derived from validated events instead of duplicated row fields. The store verifies the leased pathname and SQLite's actual open handle before applying mutable settings, and repeats actual-handle checks around writer commits and cold replay. Only version 7 opens; older, future, and nonempty unversioned databases are rejected with no migration path. Database state is separate from the `rsi-meta` platform workspace.

## Direct AI operations and artifacts

Language turns hold one `rsi.ai.language` stream for their entire tool loop and keep `rsi.agent.tools` pinned alongside it. Image generation, transcription, speech, and Realtime are separate `AgentHost` operations, each admitted through the same execution semaphore and each opening only its own service. Before opening a provider stream or reading input media, the host reserves the caller-owned `AiOperationId` in a first transaction. A second transaction records the provider's redacted Prepared snapshot, and a third records Started before the Start frame can cross the service seam. Success or failure is durably terminal before it is returned. Startup recovery closes reserved-only and prepared-only work as NotStarted and started work as OutcomeUnknown without reopening a provider.

The host-owned supervisor keeps an admitted direct operation alive if its caller future is dropped. Durable identifier reservation precedes and does not consume the provider-turn deadline; that deadline covers each unary operation and Realtime open after reservation. Each constituent provider handshake remains independently bounded by the shorter handshake timeout. Timeout, task failure, or an ordinary error before Started abandons the journal as NotStarted; after Started it records OutcomeUnknown or Failed according to whether the operation itself reached a known provider failure path. Realtime has no whole-session lifetime deadline, but each provider command and close handshake is bounded. Once a Realtime provider terminal or failure is observed, its durable terminal write belongs to a detached session task; cancelling the method that first observed it cannot cancel the write, and a later method resumes waiting for the same acknowledgement. Dropping an otherwise open session durably abandons it.

An operation identifier is a bounded duplicate-suppression tombstone, not a replay handle: terminal results are not readable through `AgentHost`. The store retains the latest 4,096 terminal identifiers in exact completion order. While retained, any reuse conflicts; once the oldest tombstone is evicted, the same identifier is admitted as a new operation and may repeat an external effect. Callers that require a longer idempotency window must retain that policy outside this runtime.

The workspace artifact store accepts only validated image or audio bytes, derives a SHA-256 identifier, writes and fsyncs a private temporary file, links it atomically, fsyncs the directory, and re-verifies every read. It is capped at 4,096 objects and 512 MiB and deliberately has no automatic garbage collector. Language/media JSON records only descriptors and artifact references. Realtime audio frames remain live-only; output audio becomes durable only after CAS commit.

A cold load uses one read transaction and one ordered `LIMIT 257` event query. It validates sequence, row count, per-event byte length, and cumulative session bytes before materializing each payload string, then applies the same session grammar used by live transitions and recovery. Recovery pages only open sessions using a 128-row keyset range query; opening the host does not scan terminal history.

## Recovery and failure containment

Opening the host scans and repairs nonterminal sessions without calling a provider. A prepared call with no dispatch record receives `NotStarted`; a dispatched call with no result receives `OutcomeUnknown`. Recovery then appends interrupted step and turn boundaries from the inside out. It never truncates history or guesses whether an effect happened.

Open does not materialize or revalidate every terminal transcript. A terminal session is fully validated whenever `run` replays that identifier or `transcript` reads it. This keeps startup proportional to unfinished recovery work rather than all retained history. There is deliberately no whole-store `verify` operation and no event hash chain: an unauthenticated chain would neither establish database provenance nor remove the need to decode and validate a transcript's typed grammar and exact request projections.

Corruption proven to belong to the selected closed session returns `AgentError::CorruptSession { session_id, .. }` and does not poison unrelated work. A stale optimistic commit precondition returns `AgentError::SessionCommitConflict` and leaves the host healthy. A read-only SQLite busy or locked result returns `AgentError::ReadUnavailable` and is likewise retryable without reopening the host. Provider transport, protocol, timeout, quota, and tool-application failures close only their operation as a reliable failed record.

Store identity or schema corruption, an unclassifiable database failure, writer commit uncertainty, or lost storage/session supervision poisons the shared host. The triggering new session receives `RecoveryRequired` when its identity is known; other active sessions stop before another durable or external-effect barrier, and subsequent operations return `HostTerminal`. A fresh `open` must reconcile the durable log before work resumes.

## Bounds

| Resource | v0 limit |
|---|---:|
| Prompt | 64 KiB UTF-8 |
| Identifier | 255 printable non-whitespace ASCII bytes |
| Concurrent executing sessions | 8 by default; configurable with `NonZeroU8` |
| Accepted `run` calls | 4 times the configured concurrent-session limit |
| Catalog | 64 tools, 256 KiB canonical bytes |
| Model steps | 8 per turn |
| Calls | 8 per step, 16 per turn |
| Transcript | 256 events per session |
| Encoded durable event | 1,600 KiB |
| Encoded event payloads | 64 MiB per session |
| Encoded service DATA envelope | 768 KiB |
| Transcript database | 512 MiB |
| Service handshake | 10 seconds |
| Model response | 60 seconds |
| Tool response | 30 seconds |
| Provider-facing turn operations | 5 minutes |
| Retained direct-AI terminal identifiers | 4,096 |

The service-envelope bound remains below `rsi-meta`'s frame limit. External JSON is closed and bounded at the protocol boundary before becoming typed in-process state. [`rsi-agent-protocol`](../protocol/README.md) and the [schemas](../../../schemas/rsi-agent/) own exact message syntax.
