---
name: Isolated frontend development with ordinary product ownership
---

## Problem

Frontend contributors need a short launch independent of personal credentials,
Service state and concurrently overwritten executables. A nested runtime path
can exceed the operating system's Unix socket pathname limit.

## Decision

The repository task builds and freezes the actual product, creates explicit
private configuration/state/workspace directories, and launches an ordinary
Application Profile with a deterministic native provider. It preserves logs and
the executable digest for inspection. Its launcher clears ambient environment
and forwards an explicit allowlist plus private HOME/XDG locations to application
and build subprocesses. Toolchain homes are supplied only inside explicit build
commands. Initial product/WASM builds and native builds each reuse a repository
cache. Native compilation and copying into per-environment artifact directories
share one file lock, because SourceRoot admits only contained artifact paths and
watchers must not exchange artifacts. This separates ambient product configuration;
build scripts still have access to Cargo configuration and registry credentials.

A separate short private runtime directory is recorded beside those development
files. Explicit directories, prepared environments and failures retain their files
for later use of the generated launcher; successful default runs remove their
private environment, runtime and artifact directories. The product's full
state identity still selects its socket. TUI changes
use the existing addon source watcher and immutable staging owner. Web renderer
changes publish an explicitly watched complete asset graph. The task supervises
process groups through exit and keeps compiler output away from the raw terminal.
Signal listeners are installed before any child starts and retained across build
and launch stages. INT, TERM, HUP and QUIT stop supervised work; once its leader
exits, remaining group members are killed before the leader is reaped. The TUI
gets its own process group and temporary terminal foreground ownership; foreground
restoration blocks SIGTTOU only around the synchronous terminal operation. Initial Cargo
builds explicitly select the repository target directory used by artifact copies,
and the launcher executable's native target triple, independent of the
contributor's Cargo target-dir and build.target configuration.

## Alternatives considered

A fake frontend runtime misses native retirement and product ownership. Mutable
loaded factories bypass Profile input convergence. Inferring Cargo watch closure
changes the explicit source producer contract. Live credentials are unnecessary
for routine layout development. Shortening the product identity digest to fit
a development socket would change an unrelated ownership boundary.

## Consequences

The first product build includes linked backends; subsequent native renderer or
WASM builds have smaller dependency closures. Worker/bootstrap edits need a new
bundle and restart. Declared watch inputs must track deliberate source changes.
Local HTTP remains an explicit development opt-in at both server and browser.

Launcher tests cover literal arguments and isolated environment. The smoke oracle
requires a matching native-provider response and completed durable Session/turn.
The Linux product CI also runs the actual development launcher smoke command,
covering generated Profiles and frozen artifact selection end to end.
The launcher signal tests use isolated nonterminal subprocesses; they do not
verify foreground terminal handoff. Local Linux/WSL PTY runs of the actual
launcher additionally exercised keyboard exit, supervisor signals, descendant
cleanup and terminal restoration. Browser product fixtures verify resource
counters through the product launcher, not through `xtask dev web`. These local
checks are distinct from the maintained CI smoke test and do not establish
native Windows or macOS behavior.
