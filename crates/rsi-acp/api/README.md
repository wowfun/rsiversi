# rsi-acp-api

Ordinary authenticated endpoint and client plugins carry the external-conversation
capability across the existing API. Endpoint launch configuration is absent. New,
resume/load, submit, cancel, close and permission answers are explicit mutations;
unknown delivery is never retried. Detaching an API client does not close a peer.

Reads are finite and bounded. Full permissions belong to one selected conversation;
resident attention contains only exact request identities and bounded titles.
History replies bind the local conversation, epoch and requested cursor. Exact
record windows use bounded hexadecimal text to preserve byte boundaries without
JSON number arrays. Generations, epochs and sequences cross JSON as canonical
decimal strings. Clients validate source bindings and aggregate page limits before
publishing the received values to application controllers.
