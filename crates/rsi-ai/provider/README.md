# rsi-ai-provider

This provider-author SDK defines Language and Image adapter traits,
`ProviderRegistration`, media resolution, cooperative abort, one-shot
`Prepared<T>`, redacted `PreparedCallSnapshot`, and bounded retry facts. It does
not expose provider HTTP/SSE syntax or perform retry scheduling. Its optional
deferred-language seam carries a closed, validated operation identity, status,
sequence cursor, explicit event-stream terminal, and
bounded parser state; accumulated model output remains caller-owned. Remote
terminal status does not close resumption by itself because polling may observe
completion before the caller has consumed any output events.
The provider SDK directly re-exports the protocol-owned deferred status,
checkpoint, and batch types; it has no parallel representation or conversion
step. Decoded durable checkpoints revalidate the complete protocol contract,
and adapters advance the same typed checkpoint in place.

Capability adapters expose a synchronous compatibility preflight with no
credential, media, filesystem, or network access. Routers run it before
resolving effect dependencies; direct adapter Prepare repeats the same check so
the provider-author seam remains safe independently.

Starting a Language or Image call returns a normalized raw stream, not a
terminal success value. The caller must pass every event through the matching
`LanguageAssembler` or `ImageAssembler` and finish that assembler before
committing success; transport EOF alone is never success.

One `PrepareContext` is constructed with the sum of its unique declared media
bytes. Construction rejects a request above the 256 MiB process resident bound;
the first Start-time resolution acquires that complete request weight atomically
before any descriptor read. It then coalesces successful resolution of identical
complete media descriptors for that prepared call. Resolution errors are not
cached, each waiter observes its own abort signal without cancelling the shared
read, and neither bytes nor in-flight work are shared across prepared calls.
Queued request-weight admission is weakly retained by its active descriptor
waiters: cancelling the last waiter removes the pending descriptor and its
semaphore position. A successfully acquired permit becomes strongly cached only
with the prepared call's resolved media.
Construction revalidates the public prepared-call snapshot once, so provider
adapters may trust its digest and identity invariants in-process.
Resolved-body digest verification runs behind fixed blocking-work admission.
An adapter that retains the context for a later control plane releases its
successful media cache after the final Start-time read; in-flight waiters keep
their own result ownership.
Media waiters have no independent wall-clock deadline: the effect owner bounds
the operation by cancelling its `AbortSignal`, which releases that waiter even
if a shared trusted reader remains non-cooperative.
