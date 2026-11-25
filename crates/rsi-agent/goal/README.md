# rsi-agent-goal

The Agent composition Goal plugin owns one bounded optional Goal domain,
effect-free state commands, ordinary-request context, pure projections and the
`report_goal` Tool. It holds no Store writer, Turn service or scheduling loop.
The Host controller owns live continuation authority and aggregate driving.
`GOAL_DOMAIN` and `GOAL_CODEC_VERSION` define the exact durable identity used by
registration, snapshot decoding and consumers that admit Goal continuations.

An application preset can set `domain_only = true` when it supplies the model's
Goal tools and prompt. The native domain, commands and projection remain
available to all clients. In this mode the factory does not register native
model context, tool policy, report processing or the `report_goal` tool.
The frozen round input carries the objective, constraints, round number and
request for verified progress. The selected preset tells the model how to
report a result through its own Goal tools.

A Goal has a positive safe-integer round maximum (at most
9,007,199,254,740,991). Omitted creation limits use the Goal plugin default,
256 unless configured otherwise. There is no aggregate Turn allowance. Round
reservation and input acceptance commit together, without refunds after acceptance.
Busy admission changes neither the allocated count nor the live lease. Resume
preserves that count. Editing the objective or maximum preserves the phase,
existing allocations and current scheduling authority. A lower cap can fall
below the number of rounds already started. An armed Goal that has exhausted
its current allowance becomes blocked with the policy code `round-limit`.
If an edit invalidates an armed Goal input before its Turn is claimed, the
controller uses the same charged round for the revised prompt. Each attempt
has its own message, reservation and settlement receipts, even when the new
text is identical. A human supersession, explicit stop or claimed Turn keeps
its original charge. The next allocation uses the current objective and cap.
The canonical accepted Session message owns the
frozen input of each earlier round. A lost acknowledgement cannot allocate
a new round.
Allocation constructs this reservation once. The reserve command compares the
caller's owner, round, message identity and text with that frozen input. Reading
a durable snapshot verifies the reserved identity against its input digest.
For an unedited reservation the current specification also verifies its text.

Application commands create, edit, resume, pause, cancel, block, clear or
explicitly complete a Goal. Creating a new Goal requires an empty slot or a
completed predecessor. Clearing the current Goal retains its command history
and stops future allocations. A revision change during an automatic round
prevents that round from reporting completion for an edited objective.
A trusted application can block an active Goal with a lower-kebab-case code
and a normalized stopping explanation. The code and explanation survive
settlement of an earlier round and cold recovery. Resume or completion clears
the blocker. The existing command and projection byte limits apply.
Explicit completion records its source, disarms future rounds and preserves any
already claimed round for settlement. Its subsequent outcome cannot reopen the
Goal. Model reports and their verification remain independent of this decision.
Internal reserve
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
contribution reads the owning Turn and intent only for a candidate report from
that Turn. Ordinary Tool results require no Goal history reads. It checks
the exact named Tool intent/result before recording a report
with its source Turn. Completion is a claim until that Turn is canonically
Completed. Blocked, Failed, PartialFailed, Interrupted and MaxTokens block an
active Goal. A failed Turn retains its unverified claim and its outcome even
when the user has already paused the Goal. In that case the Goal stays paused.
A matching completion report can still settle Completed when its in-flight
Turn completes successfully. Cancelling a blocked Goal preserves its reason.
`Goal::verified_report` returns any report disposition only when its exact source
Turn has a matching successful reservation settlement; report presence or a
Blocked phase alone does not establish verification.
Every ordinary model call receives current Goal constraints and remaining
allocated rounds through the existing context contribution.

Projection is only durable state. Reading it or a receipt never arms execution.
