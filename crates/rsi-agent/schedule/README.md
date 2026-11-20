# rsi-agent-schedule

Schedule owns a bounded reminder domain, pure reserve/settle commands, model
Tools and a projection. Its live native Host controller is supplied through a
Local contract; domain state and projection never grant execution authority.
The domain resets on fork. Host startup reschedules enabled reminders from
their committed state, including existing reminders. A consumed one-shot or
inactive reminder stays inactive. An accepted occurrence is reconciled by its
original message identity before another occurrence can reserve a Turn.
An unclaimed input left by an interrupted Host keeps its original admission.
The current Schedule callback and exact committed reservation must still match
before the Kernel can claim it. Claimed or terminal occurrences keep their
existing Turn identity.
`SCHEDULE_CODEC_VERSION` owns the exact codec shared by domain registration and
workflow continuation admission.

An operator can delete a retained reminder directly. Deletion removes its future
occurrences. Any accepted input keeps its exact identity, prompt, lifetime charge
and settlement, including across Host restart. The confirming client identifies
the reminder and its Session; deleting an absent reminder has no side effects.
For an attached or executing Session, the manager also injects a held change
notice into the next model step. Its title is quoted display data. The notice
does not dispatch a Turn. A cold Session has no notice to deliver, and a full
mailbox still permits removal of the rule.
Model-facing Tools may create, resume or delete reminders only in an initial
human-origin root Turn. An automatic Goal/Schedule round and a child cannot create or replenish
automatic work. Each mutation is authenticated against the exact started Tool,
committed through a typed domain proposal, reconciled by its canonical receipt,
and then armed in the accepting Host generation. Results distinguish
created_armed, created_disarmed and persistence_unknown. Cancellation or uncertain
persistence never arms work. A validated mutation revokes its previous timer owner
and awaits its reservation cleanup before refreshing the proposal and persisting, so a cancelled create cannot be picked up by an old timer.
If persistence rejects the mutation, existing intent remains disarmed for explicit
resume. A known committed mutation whose arming fails returns created_disarmed,
including its receipt identity, rather than presenting it as a rejected create.
Plan mode denies model-initiated scheduling mutations.

A Session holds at most 16 reminders and spends at most 100 automatic parent
Turns across its lifetime, including deleted reminders. Prompt text is limited
to 2 KiB UTF-8. `after` and `at` create one-shot reminders; `every` requires at
least one minute and keeps its original UTC anchor. A late recurring reminder
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

Model-initiated Schedule mutations guard the observed disabled plan-policy revision at Kernel
commit admission, including after disarming an earlier controller. A concurrent
policy write rejects the mutation without rewriting the policy domain.
