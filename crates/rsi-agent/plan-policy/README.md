# rsi-agent-plan-policy

An ordinary Agent-only plugin combines a typed `rsi.plan-policy` version-1 bool
domain, draft-safe `/plan` command, context contributor, monotone Tool policy and
complete projection. New drafts default to disabled; preset selection resets
that default. Durable commands use the shared revision/request receipt contract.
Forks inherit the canonical state at their declared boundary; recovery never
replays a command callback. No Kernel, Store or UI dependency belongs here.

Configuration is null for defaults or a closed object with `allow_tools`: at
most 64 unique exact Tool names. Defaults are `ask_user`, `directory_list`,
`file_read` and `output_read`.
An empty allowlist permits no Tools. Names use the Tool protocol's model-name
grammar. Configuration is frozen in the Agent generation, independent of the
typed enabled state; changing it never relaxes an already prepared call's other
approval or sandbox constraints.

The command accepts a bounded string: `on`, `off`, `toggle`, or empty text for
toggle. It proposes only a complete typed state replacement. The context
contributor records the current mode before each model retry series, including
explicit deactivation so older plan instructions cannot silently remain current.
When enabled, policy abstains only for an exact allowlisted name and denies all
other Tools before intent/start. Existing approval and sandbox requirements still
apply. The projection exposes `enabled` and the frozen ordered allowlist.

DSH plan mode informed the command/state/context composition. RSI deliberately
uses the approved allowlist contract: plan mode constrains available Tool calls,
while DSH's mode is advisory. The allowlist grants no missing Tool capability;
this plugin does not classify arbitrary shell
commands as read-only.
