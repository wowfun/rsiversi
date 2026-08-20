# rsi-ai-deepseek

This package applies DeepSeek-specific endpoint, media, and setting policy to
the shared Chat Completions implementation. It preserves `reasoning_content`
across tool turns and rejects unsupported controls during Prepare.
