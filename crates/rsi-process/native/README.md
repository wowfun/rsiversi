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

`ProcessIdentity` reads PID, creation time and TokenUser SID from one retained
process handle. `ProcessHandle` checks liveness and can terminate only the exact
generation explicitly opened for termination. PID reuse never redirects an
already retained handle. These are native mechanics; the consuming product
decides which recorded identity authorizes control.

An application detaching a resident child can clear inheritance on its borrowed
standard handles before spawning. This changes neither access nor ownership.
The flags remain cleared so concurrent child launches cannot accidentally keep
the application's original pipes open; the standard library duplicates the
explicitly selected child streams.
