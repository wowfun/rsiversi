# rsi-ai-openai-compatible

Model discovery reads `/v1/models`, using the shared OpenAI-list parser and
bounded transport. It returns candidates without
registering a deployment or inferring missing model limits.

Inference and model discovery accept an origin or a `/v1` API base without
duplicating the version prefix. Custom endpoint prefixes and explicit non-versioned paths are preserved
by the shared OpenAI endpoint joiner.

This package implements one OpenAI-compatible Chat Completions language
adapter. It translates rich messages and settings, streams reasoning/text/tool
calls and usage, preserves replay data, and rejects unsupported hosted tools or
media before dispatch. Retained tool-result messages must form the contiguous
group immediately following the assistant message that declared their call;
every declared call needs exactly one result before another role or request EOF.
Incomplete or nonadjacent histories fail with InvalidRequest during Prepare,
before media resolution, credentials or HTTP dispatch. Each start performs one HTTP
attempt.

Typed endpoint configuration selects whether semantic Developer messages use
the wire `developer` or `system` role; the default is `developer`. Concrete
provider adapters own that choice. Translation preserves message order and
content without changing the semantic request or its prepared snapshot.

An opt-in thinking switch maps the provider-supplied disabled effort ID to
`thinking.type = disabled` and omits `reasoning_effort`. Other selected efforts
enable thinking and retain their exact wire ID. The adapter has no built-in
disabled ID or provider-name policy; concrete providers configure the alias.

Usage normalization belongs to this adapter. `usage_accounting` is `inclusive`
by default: prompt tokens already include cache reads and writes. `exclusive`
requires both cache input counters and adds them with checked arithmetic.
The read counter may use any recognized alias (`prompt_cache_hit_tokens`,
`cache_read_input_tokens`, or `prompt_tokens_details.cached_tokens`); write uses
`cache_creation_input_tokens`. Operators must select the accounting declared by
their endpoint; counter values cannot reveal a misconfigured accounting mode.
In `exclusive` mode, no cache activity must be reported as explicit zero counters;
omitting either counter cannot prove zero and rejects the usage report.
Reported aliases must agree; missing optional subsets remain unknown. Impossible
or conflicting counters terminate the stream as a protocol failure.
