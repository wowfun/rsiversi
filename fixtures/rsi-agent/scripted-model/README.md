# rsi-agent-fixture-scripted-model

This native provider is the deterministic five-capability witness for keyless `rsi-agent` conformance. It uses the public `rsi-ai` Registry and `ProviderPlugin` seams and exposes language, image, transcription, speech, and interactive Realtime services. One language session first requests `echo({"text":"hello"})`, then verifies that its next request contains the committed assistant call and tool result before returning `hello`; a second returns `ready`. Media bytes and Realtime events use the same normalized grammars as production adapters.

Language calls remain pinned to their original service stream across the tool loop. The assembled runner additionally exercises each media capability and Realtime once, verifies committed artifact bytes, and then reopens completed language sessions to prove replay performs no provider work.

Its behavior test drives lifecycle, stream credit, DATA, half-close, and terminal handling through the public plugin ABI. The assembled [conformance runner](../conformance/README.md) loads the built `cdylib` through `rsi-meta`.
