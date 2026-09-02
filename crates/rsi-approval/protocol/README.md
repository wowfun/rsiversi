# rsi-approval-protocol

This package owns minimal approval requests, their typed Session/Turn/effect
subject, decisions, non-secret provenance, answerer registration, and resolver
contracts. It contains no UI, stdin, durable facts, policy engine, tool
registry, or plugin lifecycle. The subject lets a product-level live broker
route one request without making the Approval family own Agent durability.

Requests and outcomes revalidate their field bounds during deserialization, so
an external or durable decoder cannot bypass the same typed contract enforced
by the live service.
