# rsi-commands-protocol

This package owns bounded explicit command requests/results and process-local
handler/runtime contracts. It contains no chat parsing, registry
implementation, persistence, authorization, or plugin lifecycle.
Command results bound both text and the aggregate count of durable Media
references at construction, deserialization, and runtime return boundaries.
Requests and descriptors likewise revalidate their name and text bounds during
deserialization.
