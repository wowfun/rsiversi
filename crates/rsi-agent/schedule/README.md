# rsi-agent-schedule

Schedule owns a bounded reminder domain, pure reserve/settle commands, model
Tools and a projection. Its live native Host controller is supplied through a
Local contract; domain state and projection never grant execution authority.
The domain resets on fork. Restart retains intent but creates no live timers.

Only a Tool in an initial human-origin root Turn may create, resume or delete
reminders. An automatic Goal/Schedule round and a child cannot create or replenish
automatic work. Each mutation is authenticated against the exact started Tool,
committed through a typed domain proposal, reconciled by its canonical receipt,
and then armed only in the still-live Host generation. Results distinguish
created_armed, created_disarmed and persistence_unknown. Cancellation or uncertain
persistence never arms work. A validated mutation revokes its previous timer owner
and awaits its reservation cleanup before refreshing the proposal and persisting, so a cancelled create cannot be picked up by an old timer.
If persistence rejects the mutation, existing intent remains disarmed for explicit
resume. A known committed mutation whose arming fails returns created_disarmed,
including its receipt identity, rather than presenting it as a rejected create.
Plan mode denies scheduling mutations.

A Session holds at most 16 reminders and spends at most 100 automatic parent
Turns across its lifetime, including deleted reminders. Prompt text is limited
to 2 KiB UTF-8. `after` and `at` create one-shot reminders; `every` requires at
least five minutes and keeps its original UTC anchor. A late recurring reminder
coalesces missed occurrences into one Turn, then advances to the next anchored
time. Arithmetic is checked. One due reminder reserves through the shared idle
continuation admission; Busy neither spends budget nor revokes the owner.
Ordinary Tool execution remains available during the resulting finite Turn.

Resume selects explicit reminder identities without resetting the lifetime
budget. Selecting the reminder of an unsettled accepted reservation also permits
cleanup after restart, including a consumed one-shot or exhausted allowance; it
never reactivates a consumed reminder or refunds an accepted round. An overdue one-shot fires at most once. A completed/failed/cancelled
accepted round remains charged. Human input has priority and eligible Goal and
Schedule owners alternate. UTC time and timer waits are injectable; deterministic
tests never use real credentials or long sleeps.

Schedule mutations guard the observed disabled plan-policy revision at Kernel
commit admission, including after disarming an earlier controller. A concurrent
policy write rejects the mutation without rewriting the policy domain.
