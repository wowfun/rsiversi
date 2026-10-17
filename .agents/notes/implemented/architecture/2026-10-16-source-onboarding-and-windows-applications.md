---
name: Source onboarding and native Windows applications
---

## Problem

The Linux-only Python publisher and developer-oriented entry documentation do
not provide an ordinary-user source installation. Windows ACP has embedded Host
ownership, but Web and terminal applications do not share that implementation.
Historical native probes and the DSH reference reject Node piped grandchildren
under the proposed Low integrity write-restricted token.

## Decision

Documentation verification checks English/Chinese RSI user-page pairs, reciprocal
links and identical platform-labelled command examples. This is a narrow product
tutorial contract, not a repository navigation inventory or a translation mandate
for architecture documents. Platform integration checks execute applicable examples.
Fence language must match the marked shell so translated examples cannot agree
on an incorrectly labelled command block.
The gate uses CommonMark links and fences throughout the RSI user subtree and
reports independent marker, comparison, identity and filesystem errors together.
An invalid example does not suppress checks of the remaining examples.

Keep executable tooling in apps and user tutorials in the RSI product. Shell
entrypoints diagnose prerequisites before Rust exists and launch published
binaries directly. Rust owns frozen-source publication and generation leases;
a bounded text current-generation record replaces directory symlinks. English
user pages have matching `.zh.md` translations, updated together; owning English
contracts remain authoritative. Root documents link to the product tutorial.

Windows 11 24H2+ x64 uses embedded Host ownership for Web and TUI, one owner per
HostPaths. Windows Terminal is the supported TUI host, including Git Bash.
PowerShell 5.1 and 7 and Git Bash support source preparation and publication.
Published launch needs neither Cargo nor Python. Product support does not imply
Windows daemon, Browser, Automation, SSH, PTY or workspace-review support.

Local Web must preserve control progress after opening conversation subscriptions.
Actual browser acceptance exhausted all six HTTP/1 connections after the first
reply, blocking history refresh and the next submission. Loopback read subscriptions
therefore use a bounded pull-based WebSocket transport in the API family. It retains
device/Origin/generation fencing and existing API codecs and admission. Finite calls
and mutations remain HTTP; remote HTTPS retains its HTTP/2 requirement. Increasing
timeouts cannot resolve a pool occupied by indefinitely idle subscriptions, and
silently trusting local TLS certificates would introduce a separate machine-wide
trust operation into first launch.

AppContainer remains a feasibility investigation with official tools in explicit
isolated native probes. Native evidence shows that official Node's piped child
creation does not finish under AppContainer while its unconfined control passes.
This necessary gate stops production adoption; successful workspace writes and
outside-write denial alone do not establish the complete sandbox contract.
No elevation, sandbox accounts, patched Node, automatic unconfined replay or
production backend is authorized by passing a component test. A future partial
provider requires explicit consumer opt-in and a separately reviewed contract.
Standing workspace grants remain a preference for that future design, not
permission to apply unvalidated Low labels now. Shell wrappers do not introduce
a PowerShell model tool.

## Alternatives considered

A Python rewrite retains another user prerequisite and separate Windows locking.
Cargo-based launch holds the build tool executable and may rebuild during start.
winpty adds a second TUI transport without establishing paste fidelity. The DSH
token's known Node failure cannot be repaired by changing the parent console.
Dedicated sandbox accounts require elevation; custom runtimes impose additional
distribution and maintenance requirements. Both are outside the chosen scope.

## Consequences

Native tests cover shell argument boundaries, Unicode paths, publication
replacement under Windows sharing denial, live generation leases and retained
service observations. ConPTY tests cover paste, resize, cancellation and exact
mode restoration. A ConPTY test does not establish Windows Terminal presentation;
visual and provider checks remain separate, opt-in evidence using isolated state.
Windows CI runs the source-entry and build-identity targets with repository tools,
and the application catalog with CLI targets. These hosted checks provide
regression evidence; they do not replace ordinary-user local or visual acceptance.

The lease lives outside the generation because Windows prevents renaming a
directory containing an open lock file. A private builder executable allows
concurrent Cargo relinking while one build owns publication. The initial release
build may exceed an hour on a laptop; ordinary launch does not compile.
Shell publication selection deliberately introduces no persistent supervisor or
extra locking executable. Native startup establishes the pin and rechecks the
managed marker while holding it; a delayed launch that loses to collection fails
closed and can be explicitly restarted. Rechecking current in GC cannot protect
a launcher that already selected an older generation. Content verification stays
enabled for debug and release; bounded file hashing reduces peak memory without
weakening the frozen-source claim.
Capture streams regular files through temporary output and compares digests before
replacement, preserving unchanged timestamps without retaining whole-file copies.
A 64 MiB per-file limit also bounds accidentally included untracked artifacts;
it is checked before capture and while reading in case an input grows. Generated
large inputs belong in ignored paths, rather than relying on available process memory.
Addon fixture preconditions run before claiming the caller's report directory.
Its success receipt follows browser verification; failed work keeps stage evidence
without presenting a completed fixture receipt. This distinguishes a built example
from a verified application fixture without introducing another receipt format.
Caught builder panics retain their failure marker but skip automatic GC: a bounded
diagnostic does not establish that the interrupted operation's state is safe for
further mutations. A later ordinary invocation revalidates the durable entries.
Fixed-width generation identifiers make chronological retention agree with lexical
ordering. Readers and GC use the publisher's grammar; an unexpected directory
aborts collection before removal instead of displacing real recovery generations.
Capture exempts only the two directories written by its existing application
build stages and rejects authored inputs there. Exempting every directory with
a generated-looking name would hide uncaptured additions elsewhere; a separate
manifest protocol is unnecessary for these two application-owned outputs.

Even SetConsoleMode can wait for a stalled ConPTY host. Restoration retains one
owned worker and its handles, bounds the foreground wait, and rejects another
terminal owner while restoration is pending. That owner also waits for writer
settlement: bounding the caller must not permit mode restoration to race an active
write. Frame cancellation releases its waiter while the native worker retains
the write and cancellation ownership. Exact restoration requires the host
to resume reading; neither a permanently stalled host nor forced termination can
promise normal cleanup.
Acquisition uses the same ownership fence and a bounded caller wait; a timed-out
worker restores any acquired modes before releasing the fence. Writer admission
shares the mode lock with restoration, and refuses a second or cancelled writer.
Restoration attempts the screen and cursor sequence after writer settlement even
when the final frame was interrupted by normal cancellation.
Native repetition also stalled inside `CancelSynchronousIo` targeting the
restoration thread. The foreground budget therefore ends by retaining the worker,
without another console cancellation call on the shutdown path.

A WebSocket error or close request does not establish platform settlement.
Releasing its admission on a timer would let repeated failures exceed the socket
task bound. The existing connection cleanup deadline reports failure while the
task stays charged until the browser close event; fault-peer tests establish both
the bounded failure and eventual release.

A native ConPTY probe with output code page 437 reproduced corrupted Chinese and
non-BMP output even though Unicode input passed. UTF-8 byte writes depend on the
host code page. The dedicated writer therefore uses bounded Unicode console
writes; it does not change the parent's code page or rely on PowerShell 7's defaults.

An existing configured machine cannot prove a pristine OS installation. Terminal
and console-close behavior need native interaction. The Node gate blocks sandbox
adoption without blocking independent documentation and application work. ACL
experiments remain isolated and restore or report all owned resources. Unsupported
strong consumers continue to refuse rather than inherit weaker enforcement.

External-output admission rechecks containment after exclusive directory creation,
then writes through the canonical path. This catches an ancestor redirected into
the managed cache during creation, without claiming protection against a process
continuously mutating caller-owned ancestors. Failed containment leaves any newly
created empty directory for inspection; deleting through a raced path would make
recovery more dangerous. The Rust current-record reader uses a no-follow open and
validates the handle, avoiding a metadata/open gap at the final component.

Native Service application entry preacquires the same Host lease and injects it
into the child Profile. Deferring acquisition to a nested Profile enforced mutual
exclusion on Windows but folded the useful owner-conflict message into a generic
Profile failure. Acquiring at the application boundary preserves that diagnosis
without changing Host identity or duplicating the underlying lock authority.
