# rsi-process

`rsi-process` owns bounded process execution after a Sandbox has produced an
exact `ConfinedProcess`. [`rsi-process`](core/README.md) defines the platform-
neutral spawn, raw output, termination, and outcome seam;
[`rsi-process-local`](local/README.md) is the ordinary local provider.
The [native mechanics library](native/README.md) supplies shared Windows Job and
suspended-thread ownership; callers retain execution and cleanup policy.

Process owns no shell syntax, executable search, timeout classification, job
identity, model presentation, or sandbox-policy choice. Callers provide every
argv, environment, stdin, capture, and TERM-to-KILL grace value explicitly.

Completed output storage and retrieval belong to the [Spill family](../rsi-spill/README.md).
Process accepts its generic capture sink without depending on Session or retention policy.
