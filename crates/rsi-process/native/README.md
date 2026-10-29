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

Windows ConPTY creation accepts an exact executable, arguments, environment and
directory. It creates the client suspended, assigns the retained Job and resumes
the returned primary thread. Non-inherited pipes carry input and output; resize
and close serialize access to the console handle. The caller drains output while
closing the console, waits for the client and observes an empty Job before
releasing admission. These mechanisms manage lifetime, not filesystem access.
Canonical verbatim executable and working-directory paths are converted to
Win32 spelling only when both spellings canonicalize to the same object. A path
that would resolve differently (for example a trailing-dot directory) is refused.
This preserves exact selection while supporting Windows PowerShell's .NET startup.

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
