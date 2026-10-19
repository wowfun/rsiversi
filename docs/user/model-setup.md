# Configure a model

[中文](model-setup.zh.md)

This reference describes **Settings → Models & access** in Web. Each section
saves independently. Use the local launch link so this browser has configuration
access. If the page says read only, see [troubleshooting](troubleshooting.md).

1. Under **Credential**, choose the provider and leave **Credential slot** as
   `default` for your first setup. Paste the API key into **API key** and select
   **Save credential**. **Check credential** checks local availability; it does
   not prove that the remote provider accepts the key.
2. Under **Add provider**, choose a unique **Deployment name**, such as
   `my-deepseek`. This is your local name for a connection. Enter the provider's
   **Endpoint**, protocol and exact **Model identifier**. Use the model limits
   supplied by that provider under **Context and output limits**. Select
   **Apply provider**, then wait for the model to appear in **Default model**.
3. Select that deployment/model under **Conversation defaults**. New conversations use the
   saved default. Selecting it saves automatically; check **Operation results**
   for any error. Existing conversations retain the settings they started with.

For DeepSeek, the endpoint normally starts with `https://api.deepseek.com`.
Choose **Chat completions** when your account/model uses that API, or **Responses**
only for a deployment that supports it. Do not guess a model identifier from its
display name. The model catalog shown here comes from your saved deployment
configuration, not automatic account discovery.

Expand **Context and output limits** before applying the provider. The prefilled
numbers are examples, not discovered model capacities. `context window tokens`
is the total context capacity; `max output reserve tokens` is the model's maximum
output capacity. For DeepSeek, this maximum must also accommodate the configured
[request output default](../../crates/rsi-ai/deepseek/README.md#generation-policy).
Reducing `default output reserve tokens` does not change that request default.
If **Apply provider** reports `invalid API input`, check these limits before
changing your key; no inference request has been sent at this step.

OpenAI compatible connections additionally use **Request path**, commonly
`/chat/completions`. Only enable image input when the selected model supports it.

Credentials are stored by the native credential owner in the private RSIversi config
directory. Saved setup views never return the secret. The application does not
automatically import a repository `.env` file. Do not put keys in commands,
screenshots or shared logs.

After changing defaults, start a new conversation and send a short text message.
An authentication error, unknown-model error and local sandbox refusal have
different causes; use the corresponding [troubleshooting steps](troubleshooting.md).
