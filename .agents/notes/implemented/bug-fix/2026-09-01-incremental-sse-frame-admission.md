---
name: Incremental safe-state SSE frame admission
comment: Bound retained bytes without serializing overlapping declared maxima
---

## Problem

The SSE decoder reserved a provider's complete declared frame ceiling before
reading bytes and held that weight for the stream lifetime. Two OpenAI
Responses streams each declare slightly more than half of the process budget,
so the second could not read even a small event until the first ended. Ordinary
incremental semaphore acquisition would instead permit partial-allocation
deadlock when several frames consume the budget but none can reach its ceiling.

## Decision

Transport owns one process-wide safe-state admission scheduler measured in 256
KiB units. Each unfinished frame declares its finite maximum, acquires one unit
before body polling, and grows only if the resulting claims retain a possible
completion order. Oldest currently grantable growth waiters precede new-frame
admission, cancellation removes its ticket, and no scheduler lock crosses an
await.

Completed `SseData` values privately retain their actual units until drop.
Parsing compacts `data:` lines in the original byte vector before UTF-8
conversion and sealing. Comments, sentinels, errors, cancellation, and decoder
drop release admission through RAII.

## Alternatives considered

Reducing OpenAI Responses to a delta-sized ceiling was rejected because a
valid terminal event may contain the complete bounded response. An ordinary
incremental semaphore was rejected because large partial claims can deadlock.
Serializing all large declarations was rejected because configured maxima do
not represent actual retained bytes.

## Consequences

Overlapping 192 MiB-plus-one-byte declarations can both deliver small events
within the 384 MiB global bound, while a two-unit scheduler regression proves unsafe
partial allocation is refused. Delivered values intentionally backpressure
later frames if consumers retain them; dropping the opaque value releases its
exact lease. The existing 300 KiB terminal event and provider-specific frame
ceilings remain valid.
