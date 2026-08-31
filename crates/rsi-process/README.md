# rsi-process

`rsi-process` owns bounded process execution after a Sandbox has produced an
exact `ConfinedProcess`. [`rsi-process`](core/README.md) defines the platform-
neutral spawn, raw output, termination, and outcome seam;
[`rsi-process-local`](local/README.md) is the ordinary local provider.

Process owns no shell syntax, executable search, timeout classification, job
identity, model presentation, or sandbox-policy choice. Callers provide every
argv, environment, stdin, capture, and TERM-to-KILL grace value explicitly.
