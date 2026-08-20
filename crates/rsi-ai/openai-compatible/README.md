# rsi-ai-openai-compatible

This package implements one OpenAI-compatible Chat Completions language
adapter. It translates rich messages and settings, streams reasoning/text/tool
calls and usage, preserves replay data, and rejects unsupported hosted tools or
media before dispatch. Each start performs one HTTP attempt.
