# rsi-credentials-protocol

This package owns stable credential references, redacted resolved values, and
separate Resolve/Admin Local contracts. It contains no provider, keyring,
environment read, logging, or plugin lifecycle.

Status is a separate read capability returning configured/missing/unavailable,
effective source, editability and an optional bounded store location. Unavailable
includes a closed, safe failure category, never backend error text or document
contents. A local provider may inspect resolution internally, but the resolved
value never crosses Status. Environment credentials may be replaced by Admin.
Unavailable status does not assert that a key is absent. File is the current
stored provenance; Keyring remains a historical durable fact.

Store failures report definite pre-publication rejection separately from unknown
mutation outcomes. Callers must not replay unknown writes automatically.

Secret values are UTF-8 because current provider authentication contracts are
textual. They zero their owned allocation on drop and expose bytes only through
an explicit method.
