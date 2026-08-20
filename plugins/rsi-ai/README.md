# rsi-ai provider plugins

This standalone workspace packages the maintained native `rsi-ai` provider adapters for
`rsi-meta`. Each plugin declares only the service keys it can actually open; the shared
`rsi-ai-meta` host keeps calls pinned to the committed plugin generation.

Configuration secrets are supplied by the `rsi-meta` loader through fields marked
`x-rsi-meta-secret`. Plugins do not read process environment variables or an OS keyring.

