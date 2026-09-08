# rsi-api-client

This shared Rust library owns negotiated connection lifetime and API decoding for
native and browser connection plugins. It owns no socket, browser handle,
credential store or domain state. `ClientConnection` requires an explicit
Execution and a transport, verifies the connection description and exact operation
catalog, admits 4/4/8 calls under independent input, receiving and completed-response
byte pools, and drains
owned work on close. Each pool has a separate 2 MiB control or 64 MiB data/subscription
limit. A retained image or page does not consume the next finite call's maximum
receive reservation. Completed retention can still reject a reply when the
application holds too much data. Domain clients consume the resulting ApiClient capability.

`ConnectionTransport` owns one exchange's I/O, exact generation headers,
deadlines and delivery uncertainty. It receives already-admitted response storage
and a destination budget for completed responses, plus the connection's cancellation
signal. Dropping its exchange or stream must
release the corresponding local I/O. A returned stream has no hidden unbounded
queue; the shared connection supervises one-item delivery and retains its call
slot until that driver settles. Admitted mutation jobs survive response-waiter
drop. Connection retirement fences admission and cancels local exchanges; it
never stops the remote deployment or undoes a remote mutation.

Finite decoding acquires its response reservation before receiving body bytes.
Shared HTTP response validation checks single-valued identity, content type and
length headers before body decoding. Native and browser transports supply header
values; neither owns an independent interpretation of those response fields.
JSON receives within admitted capacity; binary replies consume a fixed 16-byte length
prefix and reserve the exact combined payload before allocation. Metadata and
binary slices share their original allocation's lease. At finite EOF the decoder
compacts storage, acquires completed-retention admission and moves that allocation
before releasing its receive lease. The two pools may both be occupied during
handoff; no payload escapes its byte owner. Declared lengths, trailing
bytes, invalid JSON and premature EOF are errors.

SSE decoding consumes one frame at a time without an event queue. It accepts one
optional fixed `: ready\n\n` opening comment before any events, without allocating
domain storage. Other comments, repeated or misplaced openings are invalid. Only the API's
single-line `item`, `error`, `domain-error` and `end` frames are accepted. Domain
payload storage grows under shared receiving admission before copying, bounded by
the operation's frame maximum; common errors and framing use fixed small buffers.
Two partially received frames charge their allocated capacity rather than their
operation maxima. Completed SSE payloads compact their allocations and acquire
completed-retention admission before releasing receiving ownership. Either pool
can reject actual excess storage without waiting; retained clones and slices keep
the completed payload's lease.
JSON stays as raw Rust-validated bytes, preserving exact numbers.
Error frames require the explicit terminal `end`; EOF alone is never success.
The adapter must continue through body EOF and call `finish`, rejecting trailing
bytes after `end`. A decoder error poisons the decoder and releases partial state.

Tests split frames and binary prefixes at every byte boundary, exercise malformed
and truncated input, and verify retained capacity and last-slice ownership.

The shared response driver consumes a stream of immutable byte chunks supplied by
the transport. It handles finite/common/domain replies and explicit-end SSE,
including the five-second EOF deadline after an end frame. Native and browser
adapters retain I/O cancellation and the overall finite-exchange deadline; they
do not duplicate body decoding or error-status classification.
