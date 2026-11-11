# rsi-process-native

Shared native process ownership primitives support the local Process provider
and application development supervision. They do not select executables,
environments, confinement policy, terminal modes or product state.

On Windows an unnamed, non-inheritable Job Object uses kill-on-close and forbids
breakaway. Native creation retains the exact suspended child and primary thread
while assigning the Job before user code executes.
Failure leaves termination and reaping with the caller. `terminate` addresses the
owned Job; `active` reports actual remaining members. Callers must join children
and observe zero active members before reporting successful cleanup.
Win32 calls, record layouts and constants come from `windows-sys`; this library
owns handle lifetimes and the admission checks around those calls.

`PipeChild` creates one suspended process from an exact executable, argument
vector, environment and working directory. An optional borrowed primary token
selects `CreateProcessAsUserW`; the caller owns how that token was prepared and
retains any associated grants. Only the three explicitly supplied pipe handles
are inherited. `resume` assigns the caller's Job and uses the primary thread
returned by creation, without enumerating system threads. A failed admission
leaves the exact suspended child available for termination and reaping.
Argument quoting follows Rust's ordinary Windows `Command` encoding, including
bare switches and quoted empty arguments; pipe and console children share it.

Parent pipe endpoints use overlapped named-pipe I/O on Tokio's I/O driver;
child endpoints remain synchronous standard streams. Waiting for the direct
child uses a one-shot Windows wait registration, joined before its callback
state or process handle is released. Neither pipe I/O nor process waiting
borrows a Tokio blocking worker. These mechanics require an enabled Tokio I/O
driver. Callers still own capture limits, concurrent draining and Job settlement.
Waiting exclusively borrows a child, retaining its completed exit status. Normal
drop then only closes handles; an abandoned child is terminated and reaped.

Other platforms expose no Windows primitives. This library adds no Process
capability or admission authority; the consuming owner supplies those contracts.

Windows ConPTY creation accepts an exact executable, arguments, environment and
directory. It creates the client suspended, assigns the retained Job and resumes
the returned primary thread. Non-inherited pipes carry input and output; resize
and close serialize access to the console handle. The caller drains output while
closing the console, waits for the client and observes an empty Job before
releasing admission. These mechanisms manage lifetime, not filesystem access.
Console child waiting uses the same asynchronous registration as pipe children;
console creation, resize and closure remain synchronous Win32 operations.
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
