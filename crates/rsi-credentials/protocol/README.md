# rsi-credentials-protocol

This package owns stable credential references, redacted resolved values, and
separate Resolve/Admin Local contracts. It contains no provider, keyring,
environment read, logging, or plugin lifecycle.

Status is a separate read capability that returns only configured/missing/
unavailable, effective source and editability. It cannot return a secret or a
store diagnostic. A local provider may inspect resolution internally, but the
resolved value never crosses the Status contract. Editability reflects the
same captured-environment restriction enforced by Admin, even when the keyring
is the effective source. Unavailable status does not assert that a key is absent.

Secret values are UTF-8 because current provider authentication contracts are
textual. They zero their owned allocation on drop and expose bytes only through
an explicit method.
