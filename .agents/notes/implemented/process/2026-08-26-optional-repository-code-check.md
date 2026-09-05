---
name: Optional repository code check
comment: One extensible command with non-blocking source-structure diagnostics
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
the current check parses every repository Rust file and measures effective
lines. Files above the configured threshold are reported with their largest
direct top-level items, largest named function or method, and deepest named
function or method control flow. Warning findings do not fail the command,
while configuration, discovery, read, and syntax errors do. Independent source
errors are collected before failure so an incomplete scan never emits partial
findings or a success summary. The private implementation uses
rust-analyzer's lossless single-file syntax tree so one parse owns both line
accounting and structure without requiring the repository to build.

Code check is explicitly invoked and is not part of CI, product conformance,
documentation verification, or another required gate. It analyzes source as
written, including inactive `cfg` branches, without macro expansion, name
resolution, or a Cargo semantic model. Additional checks may add their own
configuration and implementation without adding a registry or changing the
command interface before a second varying implementation exists.

## Alternatives considered

Keeping the command under `rsi-meta` was rejected because the signal applies to
all repository code. Retaining hard limits or region baselines was rejected
because code size is a review prompt rather than sufficient evidence of an
ownership defect. Repeating Clippy's item-size policies was rejected in favor
of explaining why a file is large. Loading rust-analyzer's Cargo and semantic
model was rejected until a concrete architecture rule needs resolution across
files. A configurable adapter registry was rejected because one current check
does not justify an extension seam.

## Consequences

Contributors can inspect large files across products, tools, tests, and
fixtures and see their structural hotspots without turning current size into a
release blocker. The repository receives no automatic enforcement of these
warnings, so callers must opt in when the signal is useful. The single command
and configuration can gain deeper implementation over time without widening
their interface.
