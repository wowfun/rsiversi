# rsi-ai-meta

This package maps the five typed `rsi-ai` capabilities onto generation-pinned
`rsi-meta` streams. It owns Prepare/Prepared/Start control messages, credited
binary media frames, semantic failure terminals, lifecycle cancellation, and a
shared native `ProviderPlugin` wrapper. Concrete dylibs only turn validated
generation config into a `ProviderRegistration`.
