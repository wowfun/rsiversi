# rsi-lsp-ui

The product language UI adapts the independent `rsi-lsp` provider to ordinary
Service extensions in TUI, Web and Desktop. Its activation requires the UI
registry and language provider. Its actions additionally require a native Session
controller and Session service on the actual action target, which is selected
after activation. These are target requirements, not provider-root dependencies.

Actions derive workspace authority from that Session and use the provider's
bounded query and current-file operations. Pagination retains exact normalized
results and opening a location reads current source; neither grants edit authority.

The ordinary service UI is available through Service extensions in TUI, Web and
Desktop. Actions use the actual bound Session controller, authorize its current
workspace, query or open one bounded current file at the reported position. The
viewer labels current-file content; a language result is not an immutable file
snapshot. It exposes no rename, workspace edit, rollback or editor execution.

Pagination retains the exact normalized result and never queries the server.
Each UI provider keeps at most 64 results, with the same per-result bounds as a
query, and evicts the oldest result on insertion. A cursor is bound to the actual
Session, workspace and language provider generation. An expired or mismatched
cursor produces a visible failure; it never silently reruns a query. Repeat query
explicitly obtains a new result from current files. Opening a location still reads
and validates the current file.

Service UI read failures produce a bounded visible failure view with an explicit
New language query action. Target retirement still cancels the action. A failed
read is not reported as an empty successful query or silently retried.

