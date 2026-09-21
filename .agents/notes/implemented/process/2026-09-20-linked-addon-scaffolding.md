---
name: Independent linked addon scaffolding
---

## Problem

The native addon generator covers dynamic Portable delivery but does not show
authors how to compose a source library and their own executable using Local
contracts. Workspace packages are unpublished, so registry version examples
would not be buildable.

## Decision

The existing generator includes a linked template. RSI dependencies share one
public Git URL, a full immutable revision and an independent lockfile. The
template owns its library, launcher and public lifecycle tests. Generation into a
new directory is atomic and does not change the source repository or user settings.
The [generated-project fixture](../../../../fixtures/rsi/addon-linked-template/README.md)
executes the generator and locked test/run commands in a temporary external directory;
Linux CI checks the resulting lockfile remains unchanged.

## Alternatives considered

Absolute path dependencies are useful for the existing native development
template, but would leave a distributed source example tied to this checkout.
Copying repository implementation or including private source defeats independent
SDK acceptance. Publishing all crates is a separate release decision.

## Consequences

A generated project outside the repository builds and tests with its unchanged
lockfile, exercises all four declared addon roles, replaces a Profile, invokes
the exact Local service and verifies cleanup. The native generator retains its
own acceptance. The independent language addon additionally exercises the current
public SDK and testkit with development path dependencies and a real server.

The initial immutable revision predates uncommitted work. Its tests prove the
public distribution path only; current-tree protocol and product tests remain
separate evidence. SDK upgrades require an explicit revision and lockfile update.
