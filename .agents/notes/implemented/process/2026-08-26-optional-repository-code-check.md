---
name: Optional repository code check
comment: One extensible command with non-blocking line-count diagnostics
---

## Problem

The code-health command belonged to `rsi-meta`, scanned only its production
regions, and combined exact region baselines with a hard file-size limit. That
made a repository-wide maintainability signal look like a product conformance
contract and encouraged mechanical splitting even when cohesive ownership was
more important than file size.

## Decision

Repository tooling owns one `cargo xtask code-check` interface and its adjacent
configuration. Checks remain private implementations behind that interface;
the first check measures effective lines in every repository Rust file and
reports files above the configured threshold as warnings. Warning findings do
not fail the command, while configuration, discovery, read, and tokenization
errors do.

Code check is explicitly invoked and is not part of CI, product conformance,
documentation verification, or another required gate. Additional checks may
add their own configuration and implementation without adding a registry or
changing the command interface before a second varying implementation exists.

## Alternatives considered

Keeping the command under `rsi-meta` was rejected because the signal applies to
all repository code. Retaining hard limits or region baselines was rejected
because code size is a review prompt rather than sufficient evidence of an
ownership defect. A configurable adapter registry was rejected because one
current check does not justify a public extension seam.

## Consequences

Contributors can inspect large files across products, tools, tests, and
fixtures with one command without turning current size into a release blocker.
The repository receives no automatic enforcement of these warnings, so callers
must opt in when the signal is useful. The single command and configuration can
gain deeper implementation over time without widening their interface.
