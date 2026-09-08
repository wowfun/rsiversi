# rsi-meta-execution

This library supplies explicit execution authority for owned asynchronous tasks,
synchronous preparation jobs and monotonic deadlines. An Execution retains its
platform backend; it does not create another composition or lifetime graph.
Dropping a returned task waiter never cancels the submitted job. Backend lifetime
is an embedder responsibility: stopping the platform before owned work finishes
cannot establish successful cleanup. Deadline expiry drops only the supplied
waiter, and an already-expired deadline wins before a ready result can publish.

Native execution captures an explicit Tokio handle and creates timers in that
handle's clock domain, including when called outside an entered Tokio context.
Its synchronous jobs use the same handle's blocking pool. Platform-independent
consumers use the Backend interface; no product contract enters this library.

Browser execution requires a single-threaded Dedicated Worker and uses its
monotonic performance clock and microtask scheduler. One Worker-owned callback
and at most one platform alarm serve all pending Rust timers. Dropping a timer
removes its exact registration; delivery removes it before waking caller code.
No JavaScript object is stored in a Send/Sync value, and shared-memory WASM
threads are unsupported. Preparation runs synchronously on that Worker and is
not preemptible; timeout checks run again before publishing its result.

The embedder must keep the Worker alive until Runtime shutdown completes. A
panic/trap or platform timer failure is a failed Worker, not successful cleanup.
The browser diagnostic snapshot counts pending Rust timers and active alarms;
the one bootstrap callback lives until Worker termination.
