# rsi-ai-deepseek

This package applies DeepSeek-specific endpoint, media, and setting policy to
the shared Chat Completions implementation. It preserves `reasoning_content`
across tool turns and rejects unsupported controls during Prepare.

DeepSeek Chat has no distinct developer role. Its adapter translates semantic
Developer messages to wire `system` messages in their original positions,
preserving their text and the original prepared request identity. This does
not claim separate system/developer precedence on that provider. Agent history
and provider-neutral messages retain their original roles.
