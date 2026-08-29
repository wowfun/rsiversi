---
name: Independent CI failure domains
comment: Product-owned verification jobs with one required aggregate status
---

## Problem

One Unix matrix job combined `rsi-meta` conformance with every non-meta
workspace lint and test. A failure therefore obscured the owning product,
repeated unrelated work on both operating systems, and coupled changes to the
foundation's single conformance authority with consumer verification. Branch
protection also needs one stable status without making the aggregate a place
that silently omits a newly added job.

## Decision

CI uses independent jobs for documentation, `rsi-meta` conformance, Base
services, `rsi-ai`, `rsi-agent`, the standard `rsi` product, repository tools,
dependency audit, and Windows `rsi-meta`.
Conformance remains the only command that enumerates the foundation test
surface and runs on Linux, macOS, and Windows. Product lint and tests run in
their product jobs; the Base job owns `rsi-host` and every Base service family,
while the standard product has its own end-to-end Headless boundary. The Agent
job exercises the same feature-unified graph as its ordinary package
tests; it does not repeat a command-line feature that another workspace member
already enables. Repository-wide formatting runs once in the repository-tools
job rather than being charged to every platform-specific foundation
conformance run. Repository `code-check` is an optional diagnostic and does not
run in CI.

The always-running `ci-required` job depends on every independent contract and
fails unless each result is `success`. A repository-tool test derives the set
of top-level workflow jobs and proves that the aggregate names every other job,
so adding a job without aggregation is a test failure.

## Alternatives considered

Keeping a single workspace matrix was rejected because its apparent simplicity
hides ownership and duplicates Linux-only feature checks on macOS. Making every
individual status a branch-protection requirement was rejected because job
renames and matrix expansion would turn repository policy into a second CI
inventory. Reimplementing `rsi-meta` package enumeration in separate jobs was
rejected because it would create competing conformance authorities.

## Consequences

Failures report the owning product directly and independent jobs can execute in
parallel. Checkout, toolchain, cache setup, and some dependency compilation are
repeated across jobs in exchange for isolated evidence. This cost has not been
measured closely enough to call it small or bounded; job consolidation requires
CI timing evidence and must preserve product failure ownership. `ci-required`
remains a stable protection seam, and its topology test prevents silent
weakening when the workflow grows.
