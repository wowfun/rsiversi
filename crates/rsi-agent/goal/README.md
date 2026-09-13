# rsi-agent-goal

The Agent composition Goal plugin owns one bounded optional Goal domain,
effect-free state commands, ordinary-request context, pure projections and the
`report_goal` Tool. It holds no Store writer, Turn service or scheduling loop.
The Host controller owns live continuation authority and aggregate driving.

A Goal freezes an explicit positive round maximum and the Header's immutable
five-dimensional TurnBudget. Checked products describe automatic parent-Turn
allowances, excluding monetary/token accounting and descendant budgets. Round
reservation increments the allocated count immediately and never refunds it.
Resume preserves that count. The latest reservation freezes its deterministic
message identity, complete input and digest; uncertain acknowledgement cannot
allocate another identity or round.

Application commands create, resume, pause or cancel state. Internal reserve
and settle commands require continuation dispatch and are absent from ordinary
command discovery. Their callbacks remain pure proposals under exact domain CAS.
Reserve rejects an authenticated input differing from its deterministic proposed
reservation. Cancel may abandon a never-accepted allocation after scheduling is
revoked and mailbox absence is confirmed under the controller's operation gate.
Abandonment is a domain settlement, not a mailbox discard or a refund. Draft
Cancel settles the unpublished allocation directly. Pause retains it for Resume.
Draft first reservation belongs to the staged baseline; first acceptance
publishes the Header and baseline together.
If ordinary input publishes that baseline before its automatic input is admitted,
internal reserve binds the existing first reservation to a durable continuation
command receipt. Its round, message identity, input and allocated count stay
unchanged, including when the round maximum is already allocated.

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
