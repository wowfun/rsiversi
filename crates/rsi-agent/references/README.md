# rsi-agent-references

This owner captures, verifies and reads immutable human-selected conversation
references. It consumes mechanical SessionStore operations and the Agent-owned
wire values. The Context builder consumes only their inline previews; it never
calls this service or reads CAS. Product Session admission supplies the actual
target Header, and the model Tool obtains it from AgentCallerAuthority.

Capture uses at most 1,024 Facts and 16 MiB of encoded suffix bodies, exporting
only direct human text and visible conversation assistant text. Newer text wins
the 1 MiB export limit. Explicit metadata records omissions and exact retained
Fact coordinates. CAS stores the canonical envelope and follows retain-all
ownership, including abandoned captures. Verification binds the complete frozen
descriptor to its original target before submission. Explicit reads select a
recorded reference in the current Session or its actual inherited direct-parent
interval; they accept no arbitrary digest. Human draft previews use the same
envelope verification with the actual original target Header.

The owner retains the two most recently verified immutable CAS envelopes. Cache
hits still check the exact snapshot length, target Header, metadata and preview;
only CAS I/O, hashing and decoding are reused. Eviction affects no durable state.
Two cached envelopes and at most two admitted workers bound decoded retention.

One service generation admits two finite workers. A dropped or timed-out waiter
cancels further steps, while an already dispatched Store operation retains its
worker permit until it returns. The owner drains those tasks at retirement.
Worker failures remain distinct from caller cancellation and storage failures.
Existing Store validation preparation remains a separate integrity operation;
the suffix capture limits begin after that preparation. Responses contain at
most 64 KiB of text and an 8 KiB preview. Native fixtures test source exclusion,
limits, CAS tampering, exact retry stability, lineage and cancellation.
