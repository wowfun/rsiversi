# rsi-ai-protocol

This package is the authoritative provider-neutral semantic contract for the
Language and Image `rsi-ai` capabilities. It defines closed validated requests, normalized
events and results, strict assemblers, safe provider errors, locator-free media
descriptors, and prepared-call facts. Exact request JSON shapes live in
the [product schemas](../../../schemas/rsi-ai/README.md); aggregate and temporal
invariants remain enforced by this package.

Exact model-capacity facts are stored in `LanguageModelProfiles`, a bounded
map shared by concrete adapters. Model identifiers must be explicit; an
unknown model has no inferred or family-based fallback capacity.
Remote model catalogs retain connection failures through `ModelsError::Api`.
Their bounded pages validate count, strict order and exclusive continuation at
the client boundary without acquiring provider invocation authority.

`rsi-tools-protocol` owns freeform grammar and `rsi-media-protocol` owns
locator-free image/audio metadata validation. This package imports those
validated values into Language and Image requests and adds capability-specific
aggregate and relationship limits without defining competing grammar, MIME,
digest, size, or dimension semantics.
Each normalized language event exposes the same context-free field validation
for durable envelopes; `LanguageAssembler` adds ordering, aggregate, and
terminal grammar across the complete event stream.

`ProviderExtension` is a construction- and decode-validated closed value. Its
namespace, version, JSON value, and exact encoded length are immutable and
shared across clones; callers receive borrowed accessors rather than mutable
fields or the internal `Arc`. Its JSON object shape and field order remain the
durable wire contract. This package is also the sole authority for deferred
status, checkpoint, and batch types. Checkpoint clones share frozen call and
operation identity while advance validates only the new monotonic transition
and the typed extension's cached encoded bound. Creating or decoding the closed
extension performs its full identifier, JSON-structure, and size validation;
trusted in-process checkpoint clones and batches do not serialize or revalidate
that immutable state per event. Serialization preserves the existing six-field
wire.

Language tool declarations always retain a bounded JSON function schema and
may additionally carry one bounded provider-neutral Lark freeform projection.
Because Tools is a reusable capability with a wider identifier and JSON node
budget, the Language request boundary revalidates imported Tool definitions
against the AI name and JSON structure limits before provider I/O.
Adapters that cannot preserve freeform semantics reject the request during
Prepare; they must not silently downgrade it to a function call. Every
normalized tool call records whether the provider emitted function or freeform
syntax, so retained history keeps the matching provider wire type even if the
current tool catalog changes. Tool-call identifiers are conversation-wide
unique. A Tool message contains exactly one result, that result follows its
retained assistant call, and a call has at most one result; adapters may
therefore project historical wire kind without guessing from an ambiguous
global identifier set.

Deserializing a language profile, provider extension, message, complete request,
deferred checkpoint, independently public request setting/tool/format, media
descriptor, capability request, or provider error performs the same semantic validation as its public
construction boundary. Every successful complete-request constructor and
builder additionally enforces aggregate relationships, at most 256 media
occurrences, at most 256 MiB of declared raw media across those occurrences,
and canonical encoded size. Canonicalization trusts that closed typed invariant,
sorts the owned JSON object keys in place, and checks the actual output length.
Invalid wire, durable JSON, or public construction cannot create one of those
invalid typed values.

An Image request with a mask is an edit and therefore also contains at least
one image input. Adapters reject unsupported edits during Prepare and never
silently route a mask-bearing request to generation.


## Portable provider transport

The `portable` module owns framing for versioned native provider business
protocols. A logical JSON control packet or binary body is fragmented across
Meta Messages of at most 64 KiB payload plus a 9-byte header: kind (0 JSON, 1
binary), little-endian u32 total length and u32 offset. Fragments have exact
contiguous offsets, unchanged kind/length, no zero progress except a sole empty
packet, and no trailing bytes. JSON packets admit at most MAX_REQUEST_BYTES plus
256 KiB of provider metadata; binary packets use MAX_BINARY_CHUNK_BYTES. Receiving
can narrow its JSON ceiling for metadata-only phases, and reserves the complete
declared packet against the caller's explicit ByteBudget
before allocation. Each decoder owns one incomplete packet; malformed input
permanently closes that decoder and releases unfinished storage. Completed
packets retain byte ownership through clones until the final owner drops.

The framing preserves full validated request sizes without increasing Meta's
per-Message limit. Credential values and Media bodies use binary packets and
never enter JSON envelopes. Packet Debug reports only kind and retained byte
length. Frame lengths are byte-retention bounds, not RSS promises. Semantic
Language/Image DTO and terminal validation remain with their existing owners.


The version-1 `rsi.ai.portable` control protocol uses closed Describe,
PrepareLanguage/PrepareImage and StartLanguage/StartImage requests. Describe
provides exact bounded model declarations, including the features usable during
synchronous compatibility checks. Unsupported declared features are rejected
before credential/media access. Prepare sends the caller's redacted snapshot
and validated request; its response preserves that entire snapshot and returns
at most 64 KiB of transient provider state. Start receives that frozen request
and state exactly once through the consuming provider adapter. No native token
table or pointer is persisted by this contract.

Only Start admits Credential and Media requests. Their replies are binary
packets. A Media request identifies a descriptor already present in the frozen
input and a bounded offset/length; the bridge uses the original PrepareContext
resolver, so complete request-weight admission and digest validation remain
unchanged. Native errors carry only kind and dispatch status, without arbitrary
provider diagnostic strings. Language events use their existing typed schema;
Image headers carry output index/sequence/MIME and are followed by a separate
binary packet for each output chunk. A final semantic event and clean Portable
terminal are both required; extra events or EOF without terminal fail closed.

Control JSON must preserve its typed serialization shape. This rejects unknown
fields silently ignored by nested serde unit variants without changing the
existing Language/Image public Rust shapes. Opaque provider state stays bounded
by the normal AI JSON structure limits as well as its smaller byte ceiling.
