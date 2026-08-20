# rsi-agent

`rsi-agent` exposes the embedded [`AgentHost`](src/lib.rs) interface for one
durable, bounded language/tool turn per session plus typed image,
transcription, speech, and Realtime operations. Independent work runs under one
bounded execution policy while each language session retains serial ordering.
AI/tool routing, exact request reconstruction, SQLite barriers, artifact CAS,
retry scheduling, and interrupted-work repair remain behind that interface.

The product-wide contract, trust model, and verification policy are owned by
the parent [product documentation](../README.md).
