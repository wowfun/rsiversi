# rsi-approval

`rsi-approval` owns live approval decision routing. The
[`rsi-approval-protocol`](protocol/README.md) package defines bounded requests,
decisions, provenance, answerers, and service contracts. The ordinary
[`rsi-approval`](core/README.md) plugin owns an effect-leased answerer waterfall.

The first answer wins. An empty or all-abstaining chain returns deterministic
deny. This service never reads stdin itself and owns no durable log. A consumer
that protects an external effect must own the durable asked, decided, or
interrupted evidence; the current standard product registers no effect-bearing
Tool and does not claim that Approval is already enforced.
