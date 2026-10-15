# rsi-acp-agent

OutputRecoveries budget exhaustion returns the stable ACP max_tokens stop
reason after native settlement. Other budget dimensions retain max_turn_requests.
Incomplete output is never end_turn and recovery remains native Agent policy.

The native ACP adapter translates the bounded connection driver's stable methods
into the product Session interface. Its injected `SessionOwner` is the sole
authority for preparing private composition and MCP, creating ACP Sessions,
restoring their saved manifests, enumerating only owned Sessions and retiring
their resources. Preparation must finish before Session publication. The adapter
does not construct a second Agent store or execute commands itself.

Each connected adapter admits at most 256 preparing or attached Sessions, eight
concurrent preparations, and one operation per Session. Preparation fences the
exact restored Session; unrelated Session close never waits for another setup.
Prompt tasks are retained by the adapter independently of the requesting
handler. Setup tasks likewise retain admission and cleanup ownership if a control
request disappears; shutdown joins them before retiring the Session owner.
Setup responses wait for their owned task to release preparation and operation
admission, so the next operation does not race that task's final cleanup. An
undelivered new attachment retains its admission through private-resource cleanup.
Cancel targets the admitted message, including its pre-claim interval. Before
awaiting cancellation, the adapter drops its suspended observation stream so a
pending page cannot retain the Store read budget needed by cancellation. It
resumes from the last consumed Fact/control cursors without replaying updates;
the prompt waits for durable termination and the exact Executor controlled-work
observation. A prompt response requires both the durable Turn terminal fact and
settled controlled work; controlled-work settlement alone does not prove a durable
Turn outcome. Missing or uncertain settlement is a failure, never successful
cancellation. Shutdown stops admission, cancels all prompts and joins their work
before closing private Session resources.

Load captures one durable Fact horizon and replays ascending windows of at most
64 Facts. It reads through the Session interface and crosses the peer writer
barrier before returning. Resume performs the same private preparation without
replay. Neither method can race an active prompt. Replay contains human input,
visible conversation model text and explicit Tool records, excluding reasoning,
provider-private state and internal compaction. Oversized wire records fail the
operation explicitly; they are not silently omitted. Text and ResourceLink
prompt blocks become native text; links remain references and grant no filesystem
authority. Only allow-once and reject-once are offered for native approvals.

Session setup and model configuration return complete configOptions. The product
SessionOwner supplies the bounded model directory and exact selection command;
the adapter retains per-Session operation admission and mutation ownership even
after the requesting handler disappears. The `model` choice is canonical compact
ModelRef JSON, not a slash-separated identity. Current unavailable models remain
advertised with an explanatory description; selection revalidates an active
Language route and does not fall back. Current selection reads use the immutable
model-selection projection, not the usage-advancing metrics read. Initial creation
still requires an explicitly configured default model. Unpublished setup failures
retire their draft/private resources before releasing resident admission.
Directory enumeration reads committed route pages without describing every route.
An assignment enumerates once, describes only its chosen route, and returns those
choices with the authoritative post-command selection. Responses retain exact
encoded envelope admission; mutable catalogs are not cached across operations.
The `thought_level` select option uses ACP's standard `thought_level` category.
Its values are the current LanguageProfile's exact supported effort IDs plus
`@default`, which requests the adapter default and cannot collide with an effort
ID. The default option describes the declared effective default when known;
unknown defaults remain unknown. Models without declared choices and unavailable
routes omit the selector. A retained unsupported effort remains visible with an
unavailable description, but cannot be assigned again.
Effort assignments use the same durable model-selection command as model changes;
setup, load, resume and update notifications return the complete current state.
Unsupported values fail before mutation or provider I/O. The model-only assignment
requests the target adapter's default reasoning effort, as defined by the Agent
model-selection contract; it does not retain an explicit effort from the previous
selection. Assignment admission bounds the complete advertised configuration
before mutation, then builds the authoritative post-command response.
Configuration tasks await their update notification's flush before releasing
operation admission; the connection router owns the response drain barrier.

ToolIntent and ToolRejected expose standard programmatic name independently of
display title. Rejected calls retain input, rejection text and rawOutput in live
and replayed events. Native ToolResult outcomes retain their own semantics: a
completed background submission is not a claim that its job finished.

Tests using a supplied Session owner establish this adapter's behavior only.
Application composition, private MCP, independent SDK interoperability and live
provider evidence require their separate product acceptance tests.

The [ACP application](../../../apps/acp/README.md) owns native stdio and signal
handling; this library provides its reusable native Agent backend.
