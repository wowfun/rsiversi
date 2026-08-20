# rsi-ai-provider

This provider-author SDK defines capability-specific adapter traits,
`ProviderRegistration`, media resolution, cooperative abort, one-shot
`Prepared<T>`, redacted `PreparedCallSnapshot`, and bounded retry facts. It does
not expose HTTP/SSE/WebSocket syntax or perform retry scheduling. Its optional
deferred-language seam carries a closed, validated operation identity, status,
stream-created flag, sequence cursor, and bounded parser state; accumulated
model output remains caller-owned.
