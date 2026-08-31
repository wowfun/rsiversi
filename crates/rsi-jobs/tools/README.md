# rsi-jobs-tools

This package owns the generic model-facing control plane for process-local
Jobs. `JobsToolsFactory` registers the exact `job_output`, `job_list`, and
`job_kill` batch through `ToolRegistrarContract`; it does not own or know any
shell producer and depends only on Jobs plus the unpublished Tool registrar.

Every operation requires live turn-scoped Jobs authority. Waiting reads remain
bounded, waiting reads and killing settle promptly when their Tool call is
cancelled, terminal reads report the Job exactly once, killing joins settlement,
and provider failures are projected into the stable model-visible error codes.
Model-facing output is a bounded tail of each raw retained stream. Its offsets
and `truncated` flag describe that projected window, so a legal producer-sized
capture cannot become an invalid Tool result after a terminal read has already
reported the Job.
Both identifier-taking schemas publish the same 256-byte `job_id` ceiling that
the Jobs protocol enforces. The executor parses the bounded shape, and the Jobs
provider remains the authoritative identifier-validation boundary before any
job lookup or mutation.
The factory retains one atomic batch lease so failed activation can withdraw the
complete batch while its catalog stage is open. After sealing, the Agent
generation's immutable catalog—not this contribution lease—owns the executors
and admitted calls. Because these semantics are platform-neutral, the package
has no native target gate.
