# rsi-approval-protocol

This package owns minimal approval requests, decisions, non-secret provenance,
answerer registration, and resolver contracts. It contains no UI, stdin,
durable facts, policy engine, tool registry, or plugin lifecycle.

Requests and outcomes revalidate their field bounds during deserialization, so
an external or durable decoder cannot bypass the same typed contract enforced
by the live service.
