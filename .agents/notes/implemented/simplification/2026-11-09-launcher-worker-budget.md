---
name: Small default async worker set for the native launcher
---

## Problem

Tokio's CPU-count default starts idle async workers in every native launcher and
resident Host. CPU availability does not describe the concurrency needed by
network-bound Agent turns. This creates avoidable memory overhead for the
512 MB–2 GB Linux devices targeted by the product.

## Decision

The [CLI runtime contract](../../../../apps/cli/README.md) owns a small default
worker set and preserves Tokio's explicit environment override. The launcher
selects it; reusable product libraries retain their caller's runtime.

Source inspection of DSH's Node application and measurements of RSI's standard
Kernel support separating async scheduling from blocking work rather than
allocating one async worker per logical CPU. In an isolated Linux x86_64
35-Session workload, two workers reduced idle Host-plus-Node PSS by about
19–22 MiB relative to sixteen. Four overlapping provider requests, cancellation,
history reads and Session-isolated search results passed with both settings.
These measurements justify the default without imposing a global thread cap.

## Alternatives considered

Keeping CPU-count sizing spends memory without an observed benefit in that
workload. A single-thread runtime gives synchronous application work fewer
scheduling opportunities. Limiting the blocking pool would change a different
contract and has no supporting workload evidence. A new settings field would
duplicate Tokio's existing override.

## Consequences

The default does not guarantee a whole-device memory budget or performance for
CPU-heavy plugins. Operators can select additional async workers for their
workload. Blocking threads, Node, browser memory and retained application state
still require separate measurement. The full-client cold-search performance
gate remains independent of this resource reduction.
