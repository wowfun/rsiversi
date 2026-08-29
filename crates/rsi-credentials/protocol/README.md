# rsi-credentials-protocol

This package owns stable credential references, redacted resolved values, and
separate Resolve/Admin Local contracts. It contains no provider, keyring,
environment read, logging, or plugin lifecycle.

Secret values are UTF-8 because current provider authentication contracts are
textual. They zero their owned allocation on drop and expose bytes only through
an explicit method.
