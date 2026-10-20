# rsi-process-native

Shared native process ownership primitives support the local Process provider
and application development supervision. They do not select executables,
environments, confinement policy, terminal modes or product state.

On Windows an unnamed, non-inheritable Job Object uses kill-on-close and forbids
breakaway. The caller creates a child suspended, then calls `admit_child` or
`admit_tokio_child` before user code executes. Admission retains the exact child
handle while assigning the Job and validating/resuming its single primary thread.
Failure leaves termination and reaping with the caller. `terminate` addresses the
owned Job; `active` reports actual remaining members. Callers must join children
and observe zero active members before reporting successful cleanup.

Other platforms expose no Windows primitives. This library adds no Process
capability or admission authority; the consuming owner supplies those contracts.
