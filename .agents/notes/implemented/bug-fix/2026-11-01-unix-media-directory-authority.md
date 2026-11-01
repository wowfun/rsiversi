---
name: Retained Unix Media directory authority
---

## Problem

Final-file no-follow opens did not protect the Media CAS parent components.
An existing objects or shard symlink could redirect reads, publication and chmod
outside the configured store. Reopening absolute names also lost root identity.

## Decision

The [local backend](../../../../crates/rsi-media/local/README.md) acquires the
root ancestry without following suffix links and retains the objects directory.
Shared native-fs capabilities own component opens, exclusive staging, hard-link
publication, removal and synchronization. Unix private modes apply to opened
handles; every acquired ancestor is synced during activation, including entries
left by an interrupted creation. Publication still syncs shard and objects.

## Alternatives considered

Checking symlink metadata before path-based operations leaves a replacement race.
Canonicalizing the entire path would grant authority through unintended internal
links. Repairing a linked target's permissions would mutate unrelated state.

## Consequences

Existing internal links fail closed and are not migrated. Renaming the configured
root does not redirect a running backend. This retains directory identity, not an
exclusive lock against a same-user actor modifying entries in that directory.
Failed post-publication synchronization remains an unknown outcome; identical
content retries validate bytes and obtain a fresh durability receipt.
