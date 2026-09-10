# rsi-approval-protocol

Prepared-review working directories use the
[host-path grammar](../../rsi-workspace/path/README.md), independently of the
reviewer's platform. Reviewing a path grants no filesystem authority.

`ApprovalRequest::encoded_len` measures its canonical JSON bytes without allocating
another payload buffer; live owners use that size for aggregate admission.

This package owns minimal approval requests, their typed Session/Turn/effect
subject, decisions, non-secret provenance, answerer registration, and resolver
contracts. It contains no UI, stdin, durable facts, policy engine, tool
registry, or Runtime. Local registrar calls require the caller's narrow Meta
registration credential; they do not borrow Portable invocation authority.
The protocol-owned `MAXIMUM_APPROVAL_ANSWERERS` bounds live registrations. The subject lets a product-level live broker
route one request without making the Approval family own Agent durability.

Requests and outcomes revalidate their field bounds during deserialization, so
an external or durable decoder cannot bypass the same typed contract enforced
by the live service.

An optional bounded review describes the exact prepared effect: canonical Tool
arguments, absolute working directory, one of the requested modes `read-only`,
`workspace-write`, or `danger-full-access`, and canonical request SHA-256. The consumer freezes preparation before asking and executes that same
prepared call after permission. The review is evidence of requested policy;
actual enforcement remains part of the Tool result.

The Tool registry owns the [request digest encoding](../../rsi-tools/core/README.md).
The digest binds Tool name and arguments. The executor separately freezes cwd
and sandbox from its immutable Session Header and resolved Turn policy; they
are displayed alongside the digest and are not additional digest inputs.
