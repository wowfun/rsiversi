# rsi-agent-goal

The Agent composition Goal plugin owns one bounded optional Goal domain,
effect-free state commands, ordinary-request context, pure projections and the
`report_goal` Tool. It holds no Store writer, Turn service or scheduling loop.
The Host controller owns live continuation authority and aggregate driving.

A Goal freezes an explicit positive round maximum and the Header's immutable
five-dimensional TurnBudget. Checked products describe automatic parent-Turn
allowances, excluding monetary/token accounting and descendant budgets. Round
reservation and input acceptance commit together, without refunds after acceptance.
Busy admission changes neither the allocated count nor the live lease. Resume
preserves that count. Each reservation freezes its deterministic message identity,
complete input and digest; uncertain acknowledgement cannot allocate a new round.

Application commands create, resume, pause or cancel state. Internal reserve
and settle commands require continuation dispatch and are absent from ordinary
command discovery. Their callbacks remain pure proposals under exact domain CAS.
Reserve rejects an authenticated input differing from its deterministic proposed
reservation. Draft creation allocates zero rounds. The first reserve evaluates a
private candidate baseline and publishes that allocation, Header and exact input
in one transaction. Ordinary input publishing the draft leaves its allocation at
zero; the controller later uses the same idle admission as every durable round.


`report_goal` accepts only complete, blocked or pause plus bounded evidence/reason
text. It cannot create a Goal, increase its cap or arm a driver. The
call must be the sole, final Tool call after the model's work and checks. A post-Tool
contribution checks the exact named Tool intent/result before recording a report
with its source Turn. Completion is a claim until that Turn is canonically
Completed. Failed, PartialFailed, Interrupted and BudgetExceeded take precedence
and block; Cancelled pauses. A failed Turn retains its unverified completion
claim. Pause stops future scheduling; an already committed completion report
can still settle Completed when its in-flight Turn completes successfully.
Every ordinary model call receives current Goal constraints and remaining
allocated rounds through the existing context contribution.

Projection is only durable state. Reading it or a receipt never arms execution.
