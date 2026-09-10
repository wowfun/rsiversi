---
name: Fullscreen terminal application
comment: An application consuming Session capabilities through public interfaces
---

## Problem

Durable execution already provides mailbox acceptance, reconnectable observation,
human interaction, and historical evidence. A daily terminal client also needs
editable input, readable tool results, stable historical browsing, and selection
while execution continues. Terminal presentation must not become a second Agent
state machine or retain an unbounded copy of durable history.

## Decision

The independent rsi-terminal package exports an ordinary fullscreen application
factory selected by the `tui` Application Profile. Its input framing, state transitions, historical projection,
layout, and terminal writer have distinct internal ownership. Product behavior
lives in the [rsi contract](../../../../crates/rsi/README.md).

Language model enumeration belongs to rsi-ai: adapters expose their bounded
configured model declarations, and the router lists only committed Language
routes. Applications consume the independent LanguageModels capability without
credentials or provider I/O. Queue
body reads use the existing immutable acceptance control cursor, avoiding a
second message index or a replay subscription for a single body. Their domain
API owners select operation versions and admission classes.

The implementation independently follows the behavior demonstrated by Grok's
deterministic dispatch, logical scrolling anchors, selection, and PTY tests in
the local reference checkout. No reference source or assets are vendored.
Ratatui owns terminal cell rendering; a bounded input framer precedes Termina's
public parser because receiving a fully buffered Paste event is too late to
enforce an input byte limit.

## Alternatives considered

Terminal-owned scrollback does not provide the selected fullscreen interaction
or stable application text selection. Loading all Facts or replaying ContextFold
would couple display to model context and make attachment cost grow with history.
An arbitrary Fact window therefore remains a valid partial display projection.
The existing Host lifecycle is preserved rather than implicitly launching a
daemon. Client-side model choice applies to explicit NextTurn submissions;
changing Steer routing would require a separate Kernel contract decision.

## Consequences

The [application/client foundation](../../implemented/architecture/2026-09-06-application-client-foundation.md)
places CLI, Headless and TUI in an independent terminal package. Ordinary
application Profiles compose the connection and application plugins; the launcher
invokes ApplicationRun. Submission reconciliation and observation cursor/retry
rules share the [Rust client implementation](../../../../crates/rsi/client/README.md).
Each interactive attachment now uses an ordinary Shell-owned child Profile,
composing the terminal renderer sink and shared SessionController factory with
fresh Local identities. The application owns terminal presentation; the controller
owns submission and observation work. Generation-tagged delivery also fences
queued events when reattaching the same Session. Product Web integration remains
separate work.

The application completes the coding workflow through the public Session
interface. Deterministic tests exercise input framing, rendering,
selection, resource limits, observation reconnects, interaction races, model
enumeration, and local/UDS parity. Linux PTY tests prove terminal input and
cleanup. Visual and opt-in live-provider evidence is recorded separately from
mechanism tests, with actual platform limits stated.


A single valid Fact may exceed the retained display text budget. Exact Fact
window reads preserve access at the cost of repeatedly decoding a large Fact;
the display budget is not a process RSS promise. Clipboard delivery depends on
the selected backend and terminal; an emitted OSC52 sequence is not a confirmed
copy. Ambiguous-width characters use the same narrow policy as Ratatui. The input implementation is Unix-only. Native Windows input is unsupported;
macOS terminal behavior and host SIGKILL cleanup are not established by Linux
verification.

The independent Linux PTY harness tests paste, resize, question and approval
interaction, daemon detach/resume, foreign queue reads, cancellation, and mode
restoration. Writer fault tests isolate panic and blocked output in child
processes, so they cannot alter the test runner terminal. Native clipboard
helpers require exact bounded readback; OSC52 remains explicitly unverified.
