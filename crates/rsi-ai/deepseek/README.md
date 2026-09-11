# rsi-ai-deepseek

This package defaults to the stateless OpenAI Responses protocol at `/responses`.
Explicit `protocol = "chat-completions"` selects Chat at `/chat/completions`;
there is no automatic protocol fallback. Endpoint, media and setting admission
belong to this provider. The prepared snapshot records the selected protocol.

Responses reuses the bounded shared serializer and event parser with stateless
history: no previous-response identity, background operation or stored-response
recovery is advertised. Plain reasoning events remain distinct from visible
text. Unsupported media and custom Tool names other than `apply_patch` are
rejected during Prepare instead of relying on silent provider omission.

DeepSeek has no distinct developer instruction role (Responses treats it as
user input). Its adapter translates semantic
Developer messages to wire `system` messages in their original positions,
preserving their text and the original prepared request identity. This does
not claim separate system/developer precedence on that provider. Agent history
and provider-neutral messages retain their original roles.

The ignored `deepseek_streams_a_real_completion` live test uses `DEEPSEEK_MODEL`
when set, otherwise `deepseek-flash`. It configures and requests that exact model
without fallback. Run it explicitly with `cargo test --locked -p rsi-ai-deepseek
--test live -- --ignored --nocapture`. It requires `DEEPSEEK_API_KEY` and explicit
`DEEPSEEK_CONTEXT_WINDOW_TOKENS`, `DEEPSEEK_DEFAULT_OUTPUT_RESERVE_TOKENS`, and
`DEEPSEEK_MAX_OUTPUT_RESERVE_TOKENS` for the selected model. The test reports the
selected model and validated usage, never the credential.
