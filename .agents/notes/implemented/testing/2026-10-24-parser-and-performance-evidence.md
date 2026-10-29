---
name: Separate parser robustness and report-only performance evidence
---

## Problem

Deterministic generated tests do not explore arbitrary parser input, and existing
ignored performance probes had no routine evidence collection. Conversely, real
HTTP idle/flush tests cannot be made equivalent by pausing a Tokio clock.

## Decision

The isolated parser-fuzz fixture calls feature-gated parser-only seams without
filesystem mutation or network. Its corpus also runs deterministically in the
owning crates. The weekly/manual Linux performance workflow retains raw Store,
GUI and terminal measurements plus provenance. Mechanical correctness remains
asserted; elapsed time has no universal pass threshold. Existing paused-time
deadline-owner tests and the real HTTP transport probes remain separate evidence.
The fixture lock shares product dependency resolutions. Performance output lives
outside Cargo caches and must contain actual measurements as well as successful
process exits; otherwise a removed test filter could silently erase coverage.
Admission unit tests also run in ordinary CI with nonempty discovery. Their
platform probe is isolated from subprocess mocks because Python's platform
discovery itself can execute subprocesses on Linux.

Scheduled parser campaigns use pinned tooling, bounded inputs that include production-limit rejection cases,
and temporary generated corpus. Only bounded failure inputs and provenance are
retained; a corpus cache would add durable unreviewed state without a measured
benefit. The campaign remains separate from ordinary deterministic corpus tests.
Filtered native acceptance tests require their exact name before execution;
unfiltered target suites require nonempty ignored-test discovery. These checks
prevent empty runs from silently passing as evidence.
CI package selectors and scoped sandbox policy share small workflow-owned
helpers. Workflow contract tests check delegation and ordering; the helper tests
own preparation and EXIT restoration mechanics. The policy helper retains a recoverable ownership record across partial
setup or restoration failure instead of duplicating shell restoration loops.

## Alternatives considered

Making private parser types public broadens production API without a consumer.
Fuzzing full tools would mix grammar coverage with filesystem/provider failures.
Replacing HTTP integration waits with paused time would stop measuring real socket
delivery. A single wall-clock number on an uncontrolled runner cannot justify a
dependency upgrade or an algorithm rewrite.
An additional generic test inventory or orchestration framework would duplicate
the owning test commands. A persistent fuzz corpus and a direct libfuzzer runner
add machinery unnecessary for the existing bounded weekly campaigns.

## Consequences

Fuzzing requires Linux/nightly explicitly and keeps a separate lockfile. Scheduled
performance work spends CI time without making timing a release gate. Neither
fuzz duration nor benchmark completion proves absence of defects. Dependency
duplicates and hidden-turn accounting remain measurement questions rather than
automatic cleanup targets.
