# rsi-acp-protocol

This library validates bounded stable-v1 ACP JSON-RPC messages before schema DTO
conversion. NDJSON records contain at most 1 MiB excluding LF; blank, malformed,
batch, duplicate-key and incomplete-at-EOF records are rejected. IDs are integral
JSON numbers or nonempty strings of at most 128 bytes. Incoming JSON additionally
admits at most 65,536 nodes and depth 32. Diagnostics are categorical
and do not echo payloads, credentials, commands or provider error text.

Session setup accepts at most eight stdio MCP servers within 256 KiB total.
Commands and working directories are absolute; server names and environment keys
are unique. Unsupported transports and malformed entries reject the entire
request before any session or process exists. This precedes the upstream schema's
permissive collection deserialization. Missing capability flags mean unsupported.
Inbound typed messages are checked against schemas generated from the exact pinned
stable DTOs before deserialization, so `DefaultOnError` and `VecSkipError` cannot
erase malformed known fields. Schema compilation uses bundled local definitions;
no schema or reference is loaded from a peer or the network.
Stable `session/set_config_option` responses receive the same pre-deserialization
validation, including every returned choice. Operator startup selections are
bounded, ordered string assignments; endpoint launch authority remains with Host.
Malformed capability objects or known flags reject initialization; unsupported
nonempty additional-directory requests reject setup instead of being ignored.
List cursors and Session identities are bounded before backend dispatch.
Prompts accept bounded text and resource links. Links are references, not automatic
filesystem or network reads. Permission choices retain the peer's exact IDs and
all four standard kinds; selecting allow-always never creates local authority.

The `observation` module defines external-conversation identities and bounded
observed-history DTOs shared by Host and native/wasm clients. Connection generations,
epochs and local sequences serialize as canonical decimal strings. These identify
local observations; they are neither native Fact identities nor remote durability
receipts. The journal implementation and endpoint launch authority remain outside
this protocol library.

The external-conversation Local service exposes only configured endpoint IDs,
explicit new/resume/load, one-shot text submission, bounded observations and exact
permission replies. Its owner survives detachable clients. New local identities
reconcile only their recorded start; they never authorize a repeated prompt send.
The same contract serves direct interaction and model delegation. Endpoint launch
configuration is Local Host input and is absent from this client capability.
`EXTERNAL_AGENT_TOOL_NAME` is the shared delegation identity used by the Tool
contribution and conversation navigation hints; it is not an ACP wire method.
