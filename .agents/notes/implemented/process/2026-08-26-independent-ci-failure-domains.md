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

The complete workflow runs on pull requests, pushes to `main`, and manual
dispatch. Concurrency separates event types and identifies a pull request by
number, so a manual diagnostic cannot cancel its required checks. Feature-branch
pushes do not compete with the pull request's merge-tree verification. Document
archive validation uses the pull request base, push predecessor, or `origin/main`
for a manual run; an absent event field must not become an empty base reference.

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

Linux user-namespace policy is relaxed only for native Sandbox enforcement and
standard-product tests that activate the required backend. Compilation and
linting run first under the runner policy; each test step restores every
changed sysctl on exit. The isolated frontend smoke activates the same backend
and runs within the standard-product test step's policy lifetime, with its own
failure log emitted before restoration. The deterministic required-backend failure test also
runs without relaxing policy.

Whole-package lint/test commands establish a package's single CI owner. A
focused integration or environment preflight in a consuming product job does
not transfer that ownership; the coverage check recognizes whole-target commands
rather than treating every package selector as another owning test suite.

The Linux desktop job owns native admission, standalone-versus-paired build
rejection, frame-ACK failure, startup-close deadlines, and the normal conversation
close/restart path. Its frozen distribution uses a fresh target directory; the
budget includes that cold build rather than assuming the ordinary Cargo cache
covers it. Independent desktop fault scenarios run after a successful shared build and
sandbox preflight even when another scenario fails. Their phase diagnostics and
bounded redacted child logs are always uploaded. Sandbox policy remains active
through those scenarios and is restored by an always-running final step.
The browser job also runs the shared document typecheck and ownership
tests before product interaction.

Session API oracle/pressure evaluation has its own driver-build and execution
steps, independent of standard unit/TUI outcomes, with always-uploaded evidence.
Its execution budget includes cold oracle fault checks and three task deadlines.
Desktop diagnostic unit tests run after the native scenarios, so a diagnostic
assertion cannot suppress otherwise available WebKit evidence.

The browser job caches root workspace dependencies and installed Rust tools;
dependency audit caches installed tools without a target directory. Both use
the existing pinned Rust cache action and retain locked tool installation.
Standalone fixtures keep their own lockfiles and targets. Audit enumerates
Git-tracked lockfiles, fetching advisories for the root and reusing that database
for the remaining files.

Acceptance uploads retain diagnostics and build identities while excluding the
native executable copies used to freeze scenarios. The owning fixtures record
those hashes before execution, including failure paths; desktop uploads also
retain its distribution receipt, frozen input manifest, and build log.

The always-running `ci-required` job depends on every independent contract and
consumes the complete `needs` object, failing if it is empty or any result is
not `success`. There is no second per-job result mapping. A repository-tool test derives the set
of top-level workflow jobs and proves that the aggregate names every other job,
so adding a job without aggregation is a test failure. Behavior tests execute
the actual aggregate script with successful, failed, cancelled, skipped and
missing results.

## Alternatives considered

Keeping a single workspace matrix was rejected because its apparent simplicity
hides ownership and duplicates Linux-only feature checks on macOS. Making every
individual status a branch-protection requirement was rejected because job
renames and matrix expansion would turn repository policy into a second CI
inventory. Reimplementing `rsi-meta` package enumeration in separate jobs was
rejected because it would create competing conformance authorities.

Combining the product jobs behind a shared dispatcher was rejected: removing
setup lines would introduce conditional routing without measured critical-path
savings. Dropping browsers or the desktop's second build would discard distinct
platform and build-family evidence. Native executables dominate the measured
acceptance archives, so excluding those copies removes upload work without
weakening the scenarios or adding another packaging mechanism.

## Consequences

Failures report the owning product directly and independent jobs can execute in
parallel. Checkout, toolchain, cache setup, and some dependency compilation are
repeated across jobs in exchange for isolated evidence. This cost has not been
measured closely enough to call it small or bounded; job consolidation requires
CI timing evidence and must preserve product failure ownership. `ci-required`
remains a stable protection seam, and its topology test prevents silent
weakening when the workflow grows.
Feature branches without a pull request require manual dispatch, which GitHub
only exposes after the workflow exists on the default branch. Evidence archives
identify the tested binaries but cannot restore them for exact binary replay.
Cache and artifact timing improvements require measured runs; smaller archives
alone do not establish an application or CI latency improvement.
Job deadlines cover the sum of explicit step deadlines plus ten minutes of setup
headroom, including conditionally selected platform steps. Every browser job
command has an explicit step deadline; setup actions share the headroom. The repository budget
test enforces this conservative ceiling when steps are added.
