# rsi-agent-references

This owner captures, verifies and reads immutable human-selected conversation
references. It consumes mechanical SessionStore operations and the Agent-owned
wire values. The Context builder consumes only their inline previews; it never
calls this service or reads CAS. Product Session admission supplies the actual
target Header, and the model Tool obtains it from AgentCallerAuthority.

Version 2 envelopes separate native immutable Header bindings from product-owned
observations. Capture provenance is either the existing bounded suffix or one
exact selected record/content field, UTF-8 byte range, full original text digest
and source cutoff. Selected native originals cross the Store's forward-window
budget before decoding; observed originals are reread and authorized by their
product source owner before the trusted in-process capture boundary. Reasoning,
raw provider requests and arbitrary Tool JSON are never reference exports.
Only direct human text, conversation assistant text, and explicit Tool text
evidence can be selected. Source growth does not change an already frozen CAS
envelope. A source identity or original-text mismatch rejects capture.
Suffix, selected native and observed captures consume a non-serializable capture context
issued by the product after independently admitting source reading and target
mutation. It binds complete source coordinates and identity, receiving Header and
both admission lifetimes. This owner verifies that binding and source integrity;
admission remains current before reading and before and after CAS publication.
Host grant policy stays with the product. Protected sources and targets cannot
be captured. Equal path spellings on different machines do not share a workspace.

Capture uses at most 1,024 Facts and 16 MiB of encoded suffix bodies, exporting
only direct human text and visible conversation assistant text. Newer text wins
the 1 MiB export limit. Explicit metadata records omissions and exact retained
Fact coordinates. CAS stores the canonical envelope. Pending captures retain a
publication lease until explicit removal, successful acceptance or owner retirement,
with at most 256 captures globally and four per target Session. Verification binds
the complete frozen descriptor to its original target and returns a publication
lease held through submission or queue replacement. Cancelled attempts may release
that lease; canonical commit still rejects missing or deleting CAS objects in its
writer transaction. Explicit reads select a
recorded reference in the current Session or its actual inherited direct-parent
interval; they accept no arbitrary digest. Human draft previews use the same
envelope verification with the actual original target Header.

Fresh capture returns a private, immutable validated-envelope type before encoding
and CAS publication. Descriptor construction consumes its unchanged metadata and
preview, checking the returned CAS reference without revalidating the full text.
External and durable envelopes retain complete validation at their read boundary.

The owner retains the two most recently verified immutable CAS envelopes. Cache
hits still check the exact snapshot length, target Header, metadata and preview;
current publication ownership is reacquired from bounded Store metadata even on
cache hits; payload reads, hashing and decoding are reused. Uncached payloads still
receive complete byte integrity and envelope validation. Eviction affects no durable state.
Two cached envelopes and at most two admitted workers bound decoded retention.

One service generation admits two finite workers. A dropped or timed-out waiter
cancels further steps, while an already dispatched Store operation retains its
worker permit until it returns. The owner drains those tasks at retirement.
Capture workers return their publication and reservation to the waiter; only
successful delivery registers a pending capture. Undelivered results release both
owners. Releasing a Session closes its current reservation generation, so an
already running capture cannot publish into that released generation.
Worker failures remain distinct from caller cancellation and storage failures.
Existing Store validation preparation remains a separate integrity operation;
the suffix capture limits begin after that preparation. Responses contain at
most 64 KiB of text and an 8 KiB preview. Native fixtures test source exclusion,
limits, CAS tampering, exact retry stability, lineage and cancellation.

Every capture returns a unique `capture_id` in its snapshot descriptor. Equal
content still shares CAS bytes, while each capture retains its own publication
lease and slot. Release is idempotent by target Session and capture ID; it cannot
release another capture of the same content. The ID grants no read or execution
authority and remains harmless after submission or owner retirement.
