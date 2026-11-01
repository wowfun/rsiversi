---
name: File token reclamation follows process-wide admission
---

## Problem

File tokens consume a process-wide permit, but provider-local expiry pruning
leaves an idle generation's expired tokens charged indefinitely. An unrelated
provider cannot open files even though those tokens are no longer usable.

## Decision

Admission and token publication prune a process-wide registry of weak provider
owners. Only owners with retained tokens are registered, bounding the registry
by the token budget rather than the lifetime number of provider generations.
The registry lock precedes provider-state locks; provider-local operations never
acquire the registry while holding their state. Expiry removes retained token
references. The resource and its permit remain alive for any already admitted
reader until that reader releases them. The current contract belongs to
[Files core](../../../../crates/rsi-files/core/README.md).

## Alternatives considered

Resetting permits at expiry would undercount real in-flight reads. Per-provider
semaphores would multiply the process budget. A periodic reaper would add an
independent task lifetime and still need cross-provider ownership. Registering
every empty provider would make reclamation cost depend on unrelated generations.

## Consequences

Token admission performs a bounded scan without timers or extending fixed expiry.
An idle provider cannot indefinitely block another provider with expired tokens.
Actual readers and pending native opens retain their original permits; expiry
does not manufacture capacity while those resources remain in use.
