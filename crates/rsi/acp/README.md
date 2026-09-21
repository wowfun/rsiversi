# rsi-acp-agent

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
Cancel targets the admitted message, including its pre-claim interval;
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

Tests using a supplied Session owner establish this adapter's behavior only.
Application composition, private MCP, independent SDK interoperability and live
provider evidence require their separate product acceptance tests.

On Unix, `ApplicationFactory` owns the stdio entry and signal handling. It uses
nonblocking duplicated descriptors so cancelling an idle pipe read does not
leave a blocking Tokio stdin worker behind. Redirected regular files use owned
file I/O; retirement drains outstanding file work before releasing descriptors.
Stdout carries only NDJSON; bounded
categorical diagnostics use stderr. The Service backend is supplied through an
ordinary Local contract, and Application retirement joins protocol cleanup.
