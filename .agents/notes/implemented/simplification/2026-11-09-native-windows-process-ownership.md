---
name: Retain Windows creation handles and use asynchronous process I/O
---

## Problem

The pipe provider recovered a suspended child's primary thread by enumerating
all system threads after Rust's process launcher discarded that handle. This
added a global scan, another handle open and failure cases unrelated to the
selected child. Tokio's Windows child streams used blocking workers; ConPTY
also held a blocking worker for the entire direct-child wait. A restricted
primary token requires a native creation path that Rust's ordinary process
launcher does not expose.

## Decision

The [native Process library](../../../../crates/rsi-process/native/README.md)
retains both handles returned by Windows creation, assigns the Job while the
child is suspended, then resumes that exact thread. This follows DSH's
`win32-process/src/process.ts::spawnJobProcess` ownership sequence. The removed
thread-snapshot admission APIs have no remaining callers.

Pipe and console children share command encoding and overlapped parent pipes.
Child streams remain synchronous. Both child kinds wait through a joined
Windows registration, following Tokio's process-wait ownership model. Their
consumers retain capture budgets, cancellation and descendant settlement.
The native launcher accepts an optional borrowed token; Sandbox policy and
grant ownership remain outside this mechanics library.

## Alternatives considered

Keeping the ordinary launcher would preserve the global thread scan and need
a second native launcher for token execution. A helper process would add a
process, protocol and cleanup path. Waiting for each child on a blocking worker
would retain a thread throughout idle interactive sessions. The shared native
path avoids those costs without changing capture or Job ownership.

## Consequences

Native FFI now owns pipe child creation as well as ConPTY creation. Integration
tests cover blocked-worker independence, byte integrity, argument equivalence
with Rust's launcher, explicit handle inheritance, cancellation, exact exit
codes and reaping. Existing Process and ConPTY lifecycle tests remain the
consumer-level evidence. Argument switches must retain ordinary Rust quoting;
quoting every argument breaks `cmd.exe /c`.

Console creation, resize and close remain blocking Win32 operations. Wait
unregistration briefly joins its callback before freeing state. These are not
per-child lifetime waits. Dropping an abandoned native child still terminates
and synchronously reaps it; normal provider ownership reaps asynchronously
first. An exclusive child wait retains the exit status, so normal drop closes
handles without issuing redundant terminate and wait calls. This change
supplies a token-capable creation primitive, not a Windows
Sandbox backend or a Linux small-device memory result.
