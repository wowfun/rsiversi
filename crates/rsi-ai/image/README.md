# rsi-ai-image

This ordinary plugin owns the exact-route Image service and its private provider
registrar. Provider generations reserve routes behind a shared publication gate,
so a multi-facet provider becomes visible only after every selected registrar
accepted the same generation.

Route description validates an exact deployment/model without resolving
credentials, reading Media, or invoking provider code. Callers may therefore
reject an unavailable Image route before admitting durable work.

Image preparation resolves Credentials once, pins Media read authority only for
edit inputs, freezes a redacted snapshot, and performs no provider request.
