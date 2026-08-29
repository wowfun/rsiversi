# rsi-jobs-local

This ordinary plugin implements process-local Jobs on the caller-owned Tokio
runtime. Submission registers state before spawning, active capacity counts
only unsettled jobs, and every terminal outcome is retained by its handle.
Owner-scoped cancellation closes only the exact scope during its snapshot; a
timeout keeps that scope closed until its tracked work eventually settles.

Retirement withdraws the Local service, stops admission, cancels all active
tokens, and waits for settlement under the configured shutdown timeout. A task
that ignores cancellation can therefore make cleanup fail, but it is never
reported as stopped.
