# rsi-user-questions-protocol

A request contains an opaque Host-generation identity, Session and Turn routing
identities, and one to three questions. Each question has a unique identifier,
prompt, and up to eight suggested answers. Free text is always permitted.
Request and answer documents are each bounded to 64 KiB and revalidated at their
owning external boundary. Answers correspond to questions in request order.

The synchronous `UserQuestions` contract waits until a valid answer or
cancellation. Its read/answer control surface exposes only live pending state
and bounded settled receipts. The first valid answer wins; an identical retry
returns the same receipt and conflicting content fails. Dropping a waiter,
cancellation, or provider shutdown removes its pending request. A reconnect
inside the same Host sees the same request. A new Host never reconstructs a
suspended waiter from historical Tool Facts; the Agent Kernel repairs its Turn
as interrupted. Receipt acceptance is live evidence and does not promise that
the Agent already persisted its Tool result.

Request and answer deserialization runs the same validation as service admission.
Prompt, suggestion, and answer content remains verbatim data; terminal consumers
own control-character filtering. The standard CLI applies its terminal renderer
to all these fields, while JSONL preserves them as escaped JSON strings.
