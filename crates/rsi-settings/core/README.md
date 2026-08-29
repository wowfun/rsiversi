# rsi-settings

This ordinary plugin owns the active Settings namespace registry. It loads one
complete raw provider document before publication, validates every namespace
at registration, and resolves objects recursively in `defaults -> base -> user`
order while arrays and scalar values replace as complete values.

Failed provider writes or validation leave the published value and revision
unchanged. Once a durable write begins, a service-owned operation completes its
live-state publication even if the requesting future is dropped. The complete
raw document is updated with the same commit, so later registration cannot
reload stale activation-time state. Dropping a registration lease makes all
escaped scopes stale and defers namespace handoff until any in-flight commit
has converged. A provider panic fails that commit but still releases its
in-flight namespace ownership so retirement cannot strand the name.
