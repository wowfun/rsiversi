# rsi-agent-fixture-conformance

This runner is assembled black-box evidence for the first `rsi-agent` vertical slice. The repository gate builds the three native fixtures for the current host target; the runner stages those artifacts inside their package boundaries, locks the checked-in composition, opens the real `CompositionHost`, and runs a cloneable `AgentHost` against the routed scripted model and side-effect-free echo tool. It never invokes Cargo recursively.

Two sessions start together behind a deterministic provider barrier:

- `Use the echo tool to repeat: hello` must call `echo({"text":"hello"})`, observe `{"text":"hello"}` in the next byte-exact model request, and finish with `hello`.
- `Answer directly with: ready` must make one model request, dispatch no tool, and finish with `ready`.

The scripted model emits neither first response until both model streams have submitted their first request. Successful completion therefore proves that unrelated sessions overlapped; fixture-private observers additionally require a maximum of two concurrent model streams and two concurrent tool streams. The model sees three DATA requests in total, while the tool provider sees two catalog requests and one invocation.

Both sessions are then replayed on the same host and after reopening the workspace. Their records and transcripts must remain identical. Observer streams stay attached to the same native generations and prove that neither replay opens a product-service stream or sends additional DATA. The complete scenario uses no credentials, network, sleep-based ordering, or real user state.

Run the complete fixture gate from the repository root:

```sh
cargo xtask rsi-agent conformance
```

The conformance package alone is a scenario runner, not the build/test gate; it expects the native fixture artifacts to have been built first.
