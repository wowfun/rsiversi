# rsi-agent fixtures

This fixture workspace provides deterministic, keyless evidence for the `rsi-agent` product. The scripted model and echo tool are real native `rsi-meta` providers; the capability anchor supplies the explicit consumer identity; and the [conformance runner](conformance/README.md) assembles them through the public composition and agent façades. All four packages share one lockfile and target directory.

The fixtures are deliberately narrower than a production model or tool integration. Their only contract is to make concurrent session execution, the model-to-tool-to-model path, durable transcripts, and idempotent replay observable without credentials, network access, or external user state.
