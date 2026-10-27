---
name: Independent installation maintenance and recoverable tool delivery
---

## Problem

An installer could spend minutes compiling before rejecting an existing target.
Uninstalling through that installed executable would fail when it was damaged.
Homes are shared by installations, so prefix ownership cannot authorize deleting
credentials, sessions or another installation's runtime state.

## Decision

Bootstrap scripts preflight before build and deliver an independent Rust
maintenance tool. A lightweight product library owns bounded fixed-layout records,
PATH receipts and recovery; applications own interaction. Recorded core upgrades
replace all maintenance participants through a recoverable two-directory journal.
Legacy installations retain their existing single-file transaction.

Removal preserves homes by default. Purge requires separate consent and preserves
user-authored configuration and sources. Force explicitly authorizes fixed-file
cleanup without a record, but never weakens process, lock or traversal boundaries.
Startup provenance distinguishes installation identity from executable build and
home authority. Empty coordination anchors survive removal to preserve leases.

## Alternatives considered

Running uninstall through the main executable prevents recovery from corruption.
Building during uninstall requires a toolchain at the wrong time. A permanently
stale uninstaller cannot track product data formats. Treating a prefix as owner of
homes would delete data shared with another installation. Compatibility aliases
for the renamed bootstrap scripts add no value to the pre-release contract.

The explicit format-1 recovery requirement takes precedence over removing all
compatibility at pre-release. Unrecorded executables do not acquire deletion
ownership merely by upgrading; adoption needs a separately authorized installation.
A future transaction migration must settle existing journals before removing their
reader. This change does not introduce an adoption shortcut or a removal deadline.

Installation provenance prevents ordinary uninstall from stopping a known foreign
Host when two prefixes share a build and homes. PID/build/home evidence alone
cannot distinguish them. It is not a security boundary against another process
with the same user's write authority. Parent and runtime-copy admission each verify
bytes; that startup cost is accepted for this behavior, while read-only commands
avoid it. Older executables may reject newer metadata; using the new maintenance
tool preserves process-generation fencing rather than weakening the metadata check.

## Consequences

Core packages include a small native uninstaller and its platform launcher.
Scripts use a versioned protocol. Existing unrecorded installations are never
silently adopted. Later purge can use a separately obtained prebuilt tool.
Transaction failure retains recoverable bytes; irreversible deletion reports
partial completion rather than promising rollback.

Windows PATH notification lives in one documented Win32 call in the installation
owner, preserving raw registry types without moving product environment mutation
into the generic Files or Process families. Public Desktop launch help lives in
the catalog that publishes the external application contract; Desktop keeps its
separate internal executable options. The Host runtime path conversion removes
Windows verbatim prefixes before entering the exact native Files grammar.
