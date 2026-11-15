---
name: Independent linked addon scaffolding
---

## Problem

The native addon generator covers dynamic Portable delivery but does not show
authors how to compose a source library and their own executable using Local
contracts. Workspace packages are unpublished, so registry version examples
would not be buildable.

## Decision

The existing generator includes a linked template with an independent lockfile. The
template owns its library, launcher and public lifecycle tests. Generation into a
new directory is atomic and does not change the source repository or user settings.
The [generated-project fixture](../../../../fixtures/rsi/addon-linked-template/README.md)
executes the generator and locked test/run commands in a temporary external directory;
Linux CI checks the resulting lockfile remains unchanged.

## Alternatives considered

Absolute path dependencies tie the generated development project to its SDK
checkout. Copying repository implementation or including private source would
obscure the public SDK boundary. Publishing crates is a separate release decision.

## Consequences

A generated project outside the repository builds and tests with its unchanged
lockfile, exercises all four declared addon roles, replaces a Profile, invokes
the exact Local service and verifies cleanup. The native generator retains its
own acceptance. The independent language addon additionally exercises the current
public SDK and testkit with development path dependencies and a real server.

The [shared SDK development decision](2026-11-13-local-addon-sdk-development.md)
owns checkout lifetime and the suspension of public SDK distribution. Tests
exercise current source, including uncommitted changes, without proving a public
release is available.
