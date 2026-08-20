# rsi-ai-openai

This package implements official OpenAI adapters for language Responses,
Images, audio transcription, speech synthesis, and server-side Realtime
WebSocket sessions. HTTP and socket seams are injectable so default tests use
local deterministic servers rather than live credentials. Responses also
supports explicit background submission, one-shot poll/cancel, and resumable
SSE retrieval. The SDK performs no polling loop or hidden reconnect; each
normalized batch includes the checkpoint that a durable caller must commit
atomically with its events.
