---
name: Non-executing local native addon storage
---

## Problem

Local artifact management needs a durable selection of exact native bytes without
making installation execute a plugin or implicitly activate a new version.

## Decision

The standard product owns the [native addon store](../../../../crates/rsi/core/README.md).
Its Unix writer retains directory handles, admits bounded explicit manifests,
copies and hashes source bytes into immutable content-addressed objects, then
atomically publishes a bounded index. Installed and enabled records are separate:
reinstallation does not change the enabled artifact. Source receipts describe
publication, independently of Runtime convergence. Index reads and mutations are
non-executing, including when the source manifest declares a build command.
Read-only snapshot acquisition never initializes a missing root or repairs an
incomplete existing store. It permits pure declared-selection authoring while
preserving the same ownership and bounded index validation.

This partially supersedes the product installation restriction in the
[foundation decision](2026-08-27-foundation-first-plugin-composition.md).
Meta and its Loader retain their generic explicit-path contract. Runtime loading
uses the [exact artifact admission](2026-09-09-native-exact-artifact-admission.md)
boundary; neither installation nor uninstallation prunes Loader-owned artifacts.
The binary's explicit source commands use the same store on a joined blocking
worker and report source publication separately from runtime selection. They
never start a Host or open the Loader cache. The ordinary
[native staging plugin](2026-09-09-native-agent-catalog-staging.md) owns live selection.
The remaining [runtime management proposal](../../proposed/architecture/2026-09-08-native-artifact-management.md)
still owns command-line build/watch actions beyond the managed build service
described below.

## Alternatives considered

Loading during installation would execute trusted native code before explicit
enablement. Selecting by mutable build-output path would not preserve old enabled
bytes across reinstallation. Automatically deleting source objects on uninstall
would make persisted selection and later recovery depend on current use counts.
Source objects instead remain bounded retained cache entries; this storage API
does not implement garbage collection.

## Consequences

Cooperative writers serialize through independently opened directory locks. The
writer guard explicitly unlocks before closing: another thread may fork while a
close-on-exec descriptor is live, and the child must not extend a finished write
transaction until it reaches exec.
Private temporary files can be reclaimed under that lock; unmanaged files and
oversized or linked staging entries are rejected. A failed index publication may
leave a bounded immutable object. Directory sync failure after rename is reported
as a published receipt with uncertain directory durability, not a failed write
or an automatic rollback. These mechanics currently provide a Unix writer;
they do not establish a native Windows implementation or a live update result.

## Explicit build ownership

Build execution is a product operation over the ordinary Process and Sandbox
providers. It never reimplements child supervision or trusts direct-child exit
as group settlement. Explicit argv and an owned cancellation lifetime follow the
DSH native-command/effect pattern; RSI additionally retains its managed-group
settlement and raw bounded output contracts. The source build uses an explicit
unconfined Sandbox plan because local compiler commands commonly need user
toolchain caches outside the project. This is a chosen trusted-command policy,
not a claim of restricted enforcement. `/usr/bin/env` preserves argv-zero aliases
when the Sandbox canonicalizes its own executable path.

A captured manifest and bounded explicit input fingerprint guard publication;
no attempt is made to infer a hermetic compiler dependency graph. Source-directory
locking is cooperative and does not constrain nonparticipating editors or build
tools. Only successful settled builds install through the existing content store,
with the final input check before index publication. Failed or cancelled commands
cannot install a stale artifact. Input races can leave a bounded immutable source
object without publishing a new index, as ordinary install failures can. Enable
remains a separate source mutation.
