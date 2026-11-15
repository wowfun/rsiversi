---
name: Local addon SDK development before distribution readiness
---

## Problem

The project is not ready to publish an SDK. Requiring a publicly available SDK
revision during development couples protocol changes to premature publication
and makes a generated plugin target different code from the Host under test.

## Decision

Suspend SDK publication and published-SDK acceptance. Both maintained templates
use the current checkout's public SDK crates and keep independent lockfiles.
The generator resolves those local dependencies to absolute paths without
building or resolving dependencies. Tests use generated files unchanged, cross
the actual native Describe/Execute boundary and verify linked lifecycle behavior.
The [generator contract](../../../../crates/tools/rsi-xtask/README.md) owns the
current commands and checkout-lifetime requirements.

## Alternatives considered

Publishing a new Git revision or registry package now adds a release commitment
before the product is ready. Candidate SDK archives and a separate publication
gate would remove the PR cycle but retain distribution machinery without a
current consumer requirement. A pinned public Git revision previously allowed
relocation without publishing registry packages or the working checkout. An
unpublished commit cannot establish that consumers can fetch the SDK. A public
pin must also match the Host's Portable wire; reusing a linked template's pin
without checking that boundary does not establish native loadability.

Testing a published revision establishes compatibility with that revision.
Testing current source through temporary dependency overrides covers uncommitted
SDK changes, but changes the dependency graph being tested. Keeping both paths
requires separate artifacts and reports. Local paths let generated-project
tests exercise current source directly while publication is suspended.

## Consequences

Generated projects can move independently, but their SDK checkout must remain
available at its original path. They are development projects, not portable
SDK distributions. Third-party dependencies still need a populated Cargo cache
for offline builds. SDK dependency changes can require a project lock refresh.
The [native scaffold scenario](../../../../fixtures/rsi/addon-template/README.md)
builds unchanged generated files, relocates the project, rebuilds offline with
cached dependencies, and loads Describe/Execute in the current Host. The
[linked scenario](../../../../fixtures/rsi/addon-linked-template/README.md)
checks the generated project's lifecycle behavior and lock preservation.
Together with atomic offline generation, these checks establish the local
development contract; they make no publication claim.

Resuming public distribution requires a new readiness decision and consumer
verification against a fetchable, immutable SDK revision. Native and linked
template revisions and independent locks must be updated together when Portable
compatibility requires a new pin. The exact published dependency graph must be
fetched before the unchanged generated manifests and locks undergo relocation,
locked offline builds, native Loader Describe/Execute, and linked lifecycle
verification. Fetching dependencies can require network access; generation
itself remains offline. CI evidence for that graph must remain distinct from
current-source evidence: a local dependency override or regenerated override
lock cannot prove that the distributed SDK works with the intended Host.
