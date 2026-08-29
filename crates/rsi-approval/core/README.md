# rsi-approval

This ordinary plugin owns one ordered answerer registry and the corresponding
approval resolver. It snapshots answerer `Arc`s before await, short-circuits on
the first valid answer, and fails closed to a stable default deny when all
answerers abstain.

Cancellation stops before invoking the next answerer. An already-running
trusted answerer owns its cooperative cancellation behavior. Answerer failures
remain errors and can never become an allow decision; the effect site owns any
wall-clock deadline because the core cannot detach a non-cooperative answerer
while claiming quiescence.
