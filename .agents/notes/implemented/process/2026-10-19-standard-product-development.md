---
name: Standard product development with explicit fixture isolation
---

Partially superseded by [the unified product decision](2026-10-20-unified-product-watch.md).
Ordinary CLI homes and client-held source leases supersede the private product publication and launcher. Independent optional packages also remove implicit helper builds and whole-product pairing. Explicit fixture isolation, scrubbed build environments, actual-Origin admission and bounded process cleanup remain current.

## Problem

The isolated frontend launcher selects a test provider and custom presentation
Profiles. It helps deterministic layout work but hides ordinary configuration,
workbench and browser launch behavior. Debug code can also change ordinary
state, and publishing over managed current can replace the daily build.

## Decision

Ordinary development uses the standard product Profiles and caller-selected
state, with a private external debug publication. This accepts ordinary product
writes when the developer chooses ordinary state. Independent XDG roots make
durable-format work explicit; the launcher does not copy credentials or migrate
Stores. The [launcher contract](../../../../apps/devtools/README.md#development-supervision)
owns build inputs, process containment, cleanup and output retention.

The [isolated development decision](2026-09-10-isolated-frontend-development.md)
continues to govern Linux/WSL `fixture-dev`. Its cleared runtime environment,
generated Profiles and deterministic provider serve a different trust model.
Keeping that workflow explicit avoids adding fixture Profile admission to the
ordinary product control channel merely to consolidate launcher code.

The supervised Web overlay binds before native Origin admission and preserves
the ordinary browser authority fence. Its [Web contract](../../../../apps/web/docs/runtime.md)
owns source overlay and pairing; the [control protocol](../../../../apps/dev-control/README.md)
owns readiness and shutdown sequencing. A single parent deadline per handshake
stage avoids competing child clocks; EOF supplies an existing authoritative stop
channel without unused acknowledgment verbs. Build environment keys and private
layout are data shared by the three launchers, with executable parity checks.

The applications reuse Process-owned native containment mechanics while keeping
their execution and admission policies separate. Default lifecycle verification
uses real native test peers without product authority; opt-in product smoke uses
private HOME/XDG roots. The opt-in watched smoke exercises the real proxy/native boundary and one-use
bootstrap. Native HTTP owns the common pre-dispatch authority fence; duplicating
its authentication policy in the proxy would introduce a second policy owner.
Browser checks and provider calls remain explicit.

## Alternatives considered

Making the fixture the only workflow hides product behavior; removing it loses
credential-independent layout work. Publishing debug output into managed current
changes daily launch selection. Port probing does not establish listener
ownership, and rewriting Host or Origin bypasses the browser fence. Passing
credentials to builders or watchers is unnecessary. Running the supervisor through
`cargo run` cannot reliably recover the caller's original runtime environment.

Separate backend and frontend proxy clocks can expire in different orders.
Acknowledged stop messages add a second channel when pipe ownership already
defines session lifetime. Signaling immediately after closing watcher stdin
prevents graceful renderer publication, so EOF gets a bounded completion window.

## Consequences

Ordinary state is writable by unfinished code. Daily and development Web share
browser identity, so launches can invalidate each other's authentication. The
initial frozen native/WASM build remains expensive: Linux Web retains its release
SSH helper for product parity, and captured/live trees require separate installs.

Windows and Linux/WSL have lifecycle and explicit product verification. Native
macOS supervision remains outside this change. Incomplete cleanup retains
evidence. PowerShell can preserve an interrupted-pipeline status after Ctrl+Break
even when native descendants and output have settled.
