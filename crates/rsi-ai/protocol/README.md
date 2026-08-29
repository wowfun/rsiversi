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

`rsi-tools-protocol` owns freeform grammar and `rsi-media-protocol` owns
locator-free image/audio metadata validation. This package imports those
validated values into Language and Image requests and adds capability-specific
aggregate and relationship limits without defining competing grammar, MIME,
digest, size, or dimension semantics.
Each normalized language event exposes the same context-free field validation
for durable envelopes; `LanguageAssembler` adds ordering, aggregate, and
terminal grammar across the complete event stream.

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
