# rsi-process

`rsi-process` owns bounded process execution after a Sandbox has produced an
exact `ConfinedProcess`. [`rsi-process`](core/README.md) defines the platform-
neutral spawn, raw output, termination, and outcome seam;
[`rsi-process-local`](local/README.md) is the ordinary local provider.

Process owns no shell syntax, executable search, timeout classification, job
identity, model presentation, or sandbox-policy choice. Callers provide every
argv, environment, stdin, capture, and TERM-to-KILL grace value explicitly.

The Output API endpoint and client plugins expose only the completed-output
cache. Remote readers do not acquire process execution or Session authority.
`OutputPage` has no Serde representation: adapters encode bounded metadata and
carry its raw bytes separately, so JSON cannot expand a byte page into numbers.
