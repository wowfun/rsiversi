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
