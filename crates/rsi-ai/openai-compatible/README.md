# rsi-ai-openai-compatible

This package implements one OpenAI-compatible Chat Completions language
adapter. It translates rich messages and settings, streams reasoning/text/tool
calls and usage, preserves replay data, and rejects unsupported hosted tools or
media before dispatch. Retained tool-result messages must form the contiguous
group immediately following the assistant message that declared their call;
nonadjacent histories fail during Prepare. Each start performs one HTTP
attempt.

Typed endpoint configuration selects whether semantic Developer messages use
the wire `developer` or `system` role; the default is `developer`. Concrete
provider adapters own that choice. Translation preserves message order and
content without changing the semantic request or its prepared snapshot.
