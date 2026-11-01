# rsi-ai

This package owns the ordinary Language router. It publishes
`LanguageCallContract`, admits exact generation-bound provider registrations,
and resolves each `ModelRef` without aliases or fallback. Prepare performs no
provider I/O; Start consumes the prepared call and performs one attempt.
Language providers may additionally expose explicit deferred submission: the
caller polls or cancels once per method call and commits each normalized event
batch with its monotonic checkpoint before resuming.

Provider generations are pinned by prepared calls and cannot be silently
replaced. Credential resolution belongs to `rsi-credentials`; the router stores
only a redacted credential source in a snapshot. Call identifiers are opaque
checked decimal sequences and fail closed permanently at exhaustion.

Direct and deferred Prepare each resolve one model profile and use that same
value for request defaults, output-cap validation and the frozen snapshot.
The caller's request remains immutable, including its canonical digest and
retention weight. Output defaults belong to the captured profile, not a rewritten
request; adapters resolve them through `PrepareContext::language_output_tokens`.
The unique-media accounting pass also determines whether a Media reader is
required. Compatibility and the aggregate media bound are checked before
credential resolution or Media-service access.
