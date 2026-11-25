# rsi-agent-permission

The Session permission plugin stores an owner's selected sandbox mode and
approval prompt policy together. A selection belongs to one Session and
survives reopening it from another client. New ordinary Sessions capture the
Host owner's configured default permission at creation. Changing that default
affects future Sessions across clients. With no configured permission preset,
a Session uses its existing sandbox configuration and can ask for approval.

The native composer accepts the standard DSH names `read-only`,
`workspace-write` and `danger-full-access` through `/permission`. The
Workbench passes the resolved name and knob values from the installed preset
catalog through the same owner command.

The DSH Full access preset selects `danger-full-access` and `never`.
When a Tool would require approval, `never` returns a denial before an answerer
runs. The Tool body does not start. Session and Tool rules still determine
which calls require approval. The `ask` policy delegates to the configured
approval service. Selecting a preset does not change another Session or grant
authority to execute Host commands.

An explicitly selected policy survives an Agent-preset change. If the new
preset omits the selector, its sealed generation retains the selected state
for execution. Returning to a preset with the selector makes it editable again.

A delegated child takes its sandbox ceiling from the parent's effective
Session policy at delegation. A child started by a Tool also inherits the
calling Turn's sandbox ceiling and approval requirement. A later change in the parent applies to future
children. With the permission plugin composed, a new subagent denies
required approvals until its owner explicitly selects another policy within
that ceiling.
