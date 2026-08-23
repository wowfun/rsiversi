# rsi-ai-protocol

This package is the authoritative provider-neutral semantic contract for the
five `rsi-ai` capabilities. It defines closed validated requests, normalized
events and results, strict assemblers, safe provider errors, locator-free media
descriptors, and bounded binary wire frames. Exact request JSON shapes live in
the [product schemas](../../../schemas/rsi-ai/README.md); aggregate and temporal
invariants remain enforced by this package.

Exact model-capacity facts are stored in `LanguageModelProfiles`, a bounded
map shared by concrete adapters. Model identifiers must be explicit; an
unknown model has no inferred or family-based fallback capacity.

This package is also the single owner of freeform grammar and locator-free
image/audio metadata validation reused by higher-level agent protocols. Those
protocols may add lifecycle identifiers or a different envelope shape, but do
not define competing MIME, digest, size, dimension, or grammar semantics.

Language tool declarations always retain a bounded JSON function schema and
may additionally carry one bounded provider-neutral Lark freeform projection.
Adapters that cannot preserve freeform semantics reject the request during
Prepare; they must not silently downgrade it to a function call. Every
normalized tool call records whether the provider emitted function or freeform
syntax, so retained history keeps the matching provider wire type even if the
current tool catalog changes. Tool-call identifiers are conversation-wide
unique. A Tool message contains exactly one result, that result follows its
retained assistant call, and a call has at most one result; adapters may
therefore project historical wire kind without guessing from an ambiguous
global identifier set.

Deserializing a language profile, provider extension, message, complete request, independently
public request setting/tool/format, media descriptor, capability request, or
provider error performs the same semantic validation as its public
construction boundary. Every successful complete-request constructor and
builder additionally enforces aggregate relationships and canonical encoded
size. Canonicalization validates the complete JSON value once, then sorts its
owned object keys in place rather than recursively rebuilding subtrees. Invalid
wire, durable JSON, or public construction cannot create one of those invalid
typed values.
