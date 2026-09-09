---
name: Profile edit previews preserve source identity without write authority
---

## Problem

A local Profile editor must show the effective consequence of one source edit
before writing it. Compiling a temporary file changes include resolution and
source identity. Reconstructing evaluated trees in product code duplicates the
Profile language and can expose configuration through inspection diagnostics.

## Decision

The Profile compiler accepts prospective bytes for its explicit native root
file while retaining that file's verified canonical identity. Linked fragments,
includes, expression bounds and launch patches keep their existing owners and
order. The resulting digest matches ordinary compilation after those bytes are
published, if the remaining sources and environment are unchanged.

Compiled candidates expose the existing redacted tree shape and compare typed
trees in-process. Differences name changed aspects without retaining configuration
values. Per-source fingerprints let a product detect changes to captured includes
before publishing its selected root. These APIs perform no factory preparation,
Runtime mutation or filesystem write.

Host preview attaches its frozen fragments, launch patches and environment and
resolves every enabled proposed factory through that same frozen catalog. It
returns these identities alongside both redacted trees and source fingerprints.
An invalid old program leaves the previous tree and effective diff unavailable;
it does not prevent previewing a valid repair. An unresolved proposed factory
still rejects the preview before any preparation.

The standard product exposes a consuming local edit selected through its user
Profile catalog. It pins the Unix parent directory, retains bounded original and
proposed root bytes, and borrows the frozen Host for dependency revalidation.
The CLI compares a digest of the original source, complete prospective program
and Host composition across preview and commit processes. That digest is neither
a credential nor a persistent replay ledger. Includes and linked fragments remain
read-only through this edit authority.

Commit obtains the parent directory's cooperative lock, checks source and parent
identity, stages and syncs a private sibling, rechecks the captured program, then
replaces the selected root through the pinned handle. The receipt reports source
publication and directory durability independently of Runtime convergence. A later
activation failure never authorizes restoring the old user source.

## Alternatives considered

A temporary sibling root would give preview and committed programs different
source identities and can change relative include behavior. Parsing or emulating
Profile semantics in the product editor would create another validator. Emitting
configurations in generic tree snapshots would turn an inspection surface into
a secret disclosure path.

Locking only the replaceable source inode would allow two writers to lock different
generations of the same pathname. The parent lock remains the cooperative unit
across root replacement. Treating a directory-sync error as an ordinary write
failure would conceal a source publication that has already happened.

## Consequences

The authoring product still owns explicit writable-source selection, byte and
source conflict checks, cooperative locking, atomic replacement and reporting
publication separately from activation. A compiled candidate is not proof that
plugin preparation or activation will succeed. Memory and bundled programs have
no native writable root. The compiler's existing source reader still checks the
current root file, even though it evaluates prospective bytes during preview.
Cooperative locking cannot make an arbitrary external writer participate in an
atomic compare-and-swap. Directory handles prevent parent replacement from
redirecting publication; they do not make pathnames immutable. The product writer
is available on Unix and has no Windows implementation. Explicit source views
contain the selected document's secrets; generic Debug and effective-tree views
omit source bytes and configuration values.
