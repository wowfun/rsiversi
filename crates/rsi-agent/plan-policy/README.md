# rsi-agent-plan-policy

An ordinary Agent-only plugin combines a typed `rsi.plan-policy` version-1 bool
domain, draft-safe `/plan` command, context contributor, monotone Tool policy and
complete projection. New drafts default to disabled; preset selection resets
that default. Durable commands use the shared revision/request receipt contract.
Forks inherit the canonical state at their declared boundary; recovery never
replays a command callback. No Kernel, Store or UI dependency belongs here.

Configuration is null for defaults or a closed object with optional `review_tools`
(default true) and `allow_tools`: at
most 64 unique exact Tool names. Defaults are `ask_user`, `directory_list`,
`file_read`, `output_read`, `plan_write`, `request_plan_execution`, `todo_write`,
`workflow_read` and `workflow_cancel`. The Todo Tool changes planning
state only; it grants no file or process authority.
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


## Saved plans and human handoff

The same plugin owns a second version-1 `rsi.plan-review` domain, reset on fork.
`plan_write` replaces its saved title/body (complete encoded arguments at most
32 KiB), identified by Session, exact producing Tool effect and SHA-256 digest.
It is root-only and Exclusive. Saved plans are data; saving grants no execution
authority. The complete projection includes the current plan and last decision.

`request_plan_execution` is root-only, ExclusiveFinal and HumanInteraction.
It requires enabled plan mode, no initial structured-output contract, and the
exact current plan reference. Before parking, it freezes both domain revisions
through the started-Tool settlement seam. The human sees that plan with closed
choices `approve_execute`, `request_changes`, and `decline`; feedback is limited
to 4 KiB. Suggested answers and arbitrary prose cannot approve a plan.

The pure settlement callback verifies the exact intent, plan reference and both
revisions. One atomic Tool settlement saves the decision and, only for approval,
disables plan mode. Request-changes retains plan mode; decline concludes the
Turn without a structured output. Cancellation, a replaced plan, stale revision,
or an intervening off/on change cannot approve. Review settlement requires an
uncancelled Turn at Kernel mutation admission, serialized with cancellation;
later cancellation does not undo an already admitted commit. The broker receipt means only
that an answer was delivered; the durable domain commit is authoritative.
Recovery does not replay human answers. ACP exposes neither handoff Tool.

Even a configured allowlist cannot allow `run_code` or `run_workflow` while plan
mode is enabled. Those process capabilities are not read-only planning tools.

The ACP internal preset sets `review_tools = false`; its plan command, domain,
context and policy remain available while both human handoff Tools are absent.

Optional `workflow_read` and `workflow_cancel` remain available in plan mode for
observation and revocation. Enabling plan mode revokes any workflow bound to the
previous mode revision; it never authorizes a new program.

When `review_tools = false`, context and the effective allowlist omit the unregistered review Tools. Plan mode still requires `/plan off` before execution.
