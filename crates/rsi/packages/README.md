# rsi-packages

Core distribution publishes root-level executable exports `core` and `uninstaller`
with their platform filenames (`rsi[.exe]`, `rsi-uninstall[.exe]`), plus
the platform's declared `uninstall.sh` or `uninstall.ps1` payload. Managed
installation upgrades require the compatible complete set. Legacy single-file
core packages remain admissible for unrecorded installations; generic manifest
admission does not confer installation or deletion authority.
Offline purge obtains the writer lease and every generation lease before removing
objects and resolution metadata. Busy generations refuse admission. Coordination
anchors remain; unknown top-level store entries are not purge authority.
Admission traverses every recognized object under those leases before the
caller may remove its executable. It rejects deterministic count, depth, type
and name failures without deleting payloads. Purge repeats these checks during
removal; external filesystem changes and I/O failures can still require retry.
Purge uses the same per-object traversal bounds as staging cleanup and GC, so
every admitted object can be removed in one pass without a smaller store-wide cap.

This library owns bounded, non-executing package admission and immutable storage
below the selected state home. Applications supply downloads and source recipes;
installing a package never runs a compiler, lifecycle script or executable.
An optional `payload_tree` export carries an uncompressed RSI tree container:
the eight bytes `RSITREE1`, a little-endian u32 JSON-header length, a format-1
header with ordered FileRecords, then their exact bytes. Its export declares
`index_digest`, the SHA-256 of the exact magic, length and encoded header.
The source FileRecord still authenticates the complete transport container.
Admission streams that container into the unpublished expanded tree and retains
only its index under `content`; it does not store a second copy of the file bodies.
Payload containers cannot also be ordinary file or Web-asset exports. Files are regular,
hash-checked and immutable; links and case-colliding paths are refused. Hidden
ordinary components are allowed inside payloads for dependency-manager metadata.
Each tree has a 16 MiB header limit and paths of at most 32 components. All
trees in one package share a budget of 65,536 expanded files and directories.
The index and expanded
files together reserve the container's logical byte length once. The materialized payload retains
its complete PackageLease, so selection changes and GC cannot remove a live tree.
There is no second expansion cache or installation registry.
Ordinary acquisition authenticates each bounded retained index against the export's
commitment. That index owns the expanded files' names, lengths and digests.
It walks the expanded tree once, matching each
exact name and type to that header and hashing each file through its retained
parent directory. A missing, extra, changed or substituted entry fails the
acquisition. A missing index commitment is inadmissible; no legacy archive fallback
or in-place object conversion is provided. Windows excludes the filesystem-owned `.rsi-namespace-pin` guard
of each pinned directory.
Installation writes in archive order and holds only the current ancestor chain.
For large payloads, completed files may sync concurrently while the installer
reads the next entry. These operations share the process-wide worker budget
with verification. Every file sync must succeed before syncing directories
and publishing the object.
Resolving a `payload_tree` export returns its expanded directory; its transport
container is not an installed file export.
For large acquisitions the directory walk sends bounded batches from retained
parents to available workers. The process admits at most seven additional
payload I/O workers in total. All readers settle before the lease is published.
The `process_extension` export is a format-1 JSON descriptor, bounded to 64 KiB,
with an immutable payload working directory and at most 64 arguments. Arguments
are literal strings, the host-issued generation identity, or paths in a named
payload of this package or an exact direct dependency. It never selects an
executable or inherits an environment. Descriptor parsing and reference admission
run before resolution publication; host interface requirements remain in the
manifest. Consumers resolve paths from the leased closure, never current names.
The `package_importer` export uses the same descriptor grammar with the additional
`rsi.packages.importer` v1 requirement. Its consumer owns a short-lived process
scope. `package.import` receives format 1, an absolute private staging directory,
the caller's working directory and 1–128 nonempty specifications (at most 64 KiB
total). It returns format 1 and 1–128 distinct immediate child directory names.
The request also identifies the leased importer by package name, exact manifest
digest and direct dependencies, so generated descriptors can retain that same
implementation closure. This metadata does not grant access to other exports.
Only those staged PackageStore manifests may enter ordinary atomic admission;
importers cannot publish resolution selections or grant signed provenance.
Profiles select activation. The resolution lock records installed names, exact
manifest digests and dependency closures; it contains no enabled flag.

A format-2 manifest declares its target, every source file's relative path, length and SHA-256,
exact dependencies, named exports and required interface descriptors. Paths are
portable relative components; links, device names, case-fold collisions and
undeclared exports are refused. Publication stages all bytes, verifies the
entire dependency closure and atomically replaces one resolution lock. Failed
admission leaves the previous package selection authoritative.

Required interfaces contain protocol name and explicit semantic version. Source
fingerprints are not admission inputs. Parsing bounds bytes, decodes the closed
format once, and validates its format and cross-field rules. Failed decoding may
probe only the format to explain an unsupported historical manifest. Resolution locks and signed repository metadata retain
format 1. To leave manifest format 1, first install the new executable to a separate
empty prefix, stop old holders, remove all old selections (including core) in one
uninstall, and collect unselected objects after their leases close. Existing trust
receipts and user data are retained; install never skips old leased objects.

Official metadata is an Ed25519-signed bounded payload under an explicitly
pinned public key. It declares a monotonically increasing sequence, issue and
expiry times and target manifest hashes. Verification precedes downloads;
expired metadata, an older sequence, or different bytes at the same sequence
are rejected. Accepting verified metadata immediately persists its high-water
receipt, including when subsequent download or publication fails. Retrying the
same signed payload is allowed. Only an opaque verified repository can authorize
signed installation; each manifest must match its name, target and digest in that
repository. Durable receipts describe history and cannot authorize new content.
Explicit local files are recorded with local provenance and never acquire an
official signature claim. Signature tests use independent fixture keys.

Installed objects remain immutable. A named profile can retain exact installed
roots across process retirement. Its roots map manifest digests to package
names, so two versions of one package can coexist during handoff. A writer
compares the previous set before replacing it. Confirming an unchanged set
shares the Store's read lock and still checks the expected roots. Changes
acquire the exclusive writer lock. Roots acquire no new execution
authority. Additions must already be selected, retained by another profile,
or held by the Store's process leases. Collection retains the transitive
closure of every profile's roots.
The Host Store bounds the number of profiles and roots per profile to its
package count limit. An empty set removes the profile's reservation.
Candidate admission publishes immutable objects and acquires process closure
leases under the same Store writer lock and producer fence. It changes neither
the CLI selection nor persistent profile roots. The returned owner holds every
produced root and its transitive dependencies until discarded or retained by a
profile. Dependencies must be in the transaction or already admitted to the global
selection, invoking profile closure or a live process lease. Process leases reuse
exact live generations without repeating installation's content verification.
Owners may namespace persistent root sets as `owner/profile`.

A consumer leases its complete resolved closure before exposing paths.
Before exporting a path, Unix verifies that the object's absolute namespace still
names its retained directory. Handle-based reads survive a moved Store; exported
pathnames do not. The owner must keep the Store and its ancestors in place while
consumers retain paths (including subprocesses). A check cannot prevent a later
same-user rename. Windows namespace leases prevent the corresponding rename.
Updating the resolution lock does not mutate
already leased bytes. Garbage collection may reclaim only unselected objects
whose exclusive lease can be acquired. Home reset preserves this store and
its trust receipts.

Executable launch transfers the exact manifest digest, so a simultaneous
selection update cannot redirect the child to another generation. Acquisition
by digest walks that immutable manifest's dependencies, independent of current
name selections. Uninstall atomically removes names only when the remaining
selected closure is complete. GC retains selected and leased objects, removes
abandoned staging objects, and uses bounded no-follow directory-handle removal.
Trust high-water receipts survive uninstall and GC. Unknown objects are retained
and reported with their name; move them outside `objects/` before retrying GC.
The collector never deletes an unrecognized operator-owned directory.
Read-only resolution and acquisition share the coordinator; mutation and GC
require exclusive ownership. Each coordinator or generation lease explicitly
unlocks when its owning guard ends, before closing the descriptor. A concurrent
fork retaining that descriptor cannot extend the completed owner's lock.
Acquisition pins objects before releasing that coordinator.
Read acquisition validates each object's identity before creating its generation
anchor. Exclusive collection and purge admission remove unleased generation
anchors whose objects are absent, including anchors retained by a prior purge.
One repair visits at most 384 entries; a larger directory reports retryable
maintenance-incomplete progress after syncing removals. An unrecognized lease
name stops repair with an error; it is not removed. Busy leases are retained.
Writer and namespace coordination anchors are never removed.
Installation removes
abandoned `stage-<id>` trees, including a
stage interrupted before its manifest was written. It accounts for existing
and incoming digests together under the writer lease. Complete retired
generations remain available for reuse within the Store's count and byte
limits. An install that needs space reclaims unselected, unleased objects
outside its candidate set until the reservation fits. Explicit GC can
reclaim these objects at any time. Recovery enumeration allows the 256
committed objects plus at most 128 interrupted staging names. Installation
syncs each file after writing it. Once all files are staged, it syncs their
directories from leaves through the object root before publication.
If the incoming and already selected objects alone exceed capacity, the
reservation fails before evicting reusable generations.
If staging exhausts filesystem space or quota, installation discards the
incomplete stage and collects eligible retired generations outside its inputs.
When that releases an old object, it retries staging once under the same
writer lease, before the producer's publication check.

Limits are enforced before parsing or allocation: manifests and repository
payloads at most 1 MiB; at most 128 selected packages, 256 objects, 4,096 files
per package, 4 GiB per file and 16 GiB across installed payload files.
File hashing and copying stream bounded chunks; these disk limits are not
allocation sizes. The file limit admits ordinary unstripped debug executables,
and the store limit leaves room for retained core generations during upgrade.
Native filesystem handles admit all durable path transitions; descendants are
opened relative to their retained parent, never by re-resolving absolute paths.
Windows requires
TokenUser-owned private fixed-NTFS roots; Unix roots must be owned and private.
Selected Store and install-source paths must have physical parents; the Store
does not follow system temporary-directory aliases or caller-selected symlinks.

Source producers may use `install_checked` to revalidate their captured inputs
and cancellation immediately before the atomic resolution write. This callback
runs under the package writer lease after complete object staging. Rejection
leaves the previous selection intact; independently accepted trust receipts remain.
Newly staged immutable objects from a failed transaction and its unfinished
stages are reclaimed before returning, retaining any selected or leased generation.
If resolution publication reports a durability error after rename, cleanup reads
the actual resolution again before deciding which objects are unselected. The callback is an in-process producer fence,
not package-provided executable code.
When publishing a selection, the Store reuses an existing candidate whose
immutable object passes verification, including an unselected generation.
An unselected, unleased candidate that fails verification can be reclaimed
and staged again from the supplied source.
One lease can materialize payload exports of its exact direct dependencies.
Callers retain the selection lease while using those paths.
Process acquisition authenticates bounded manifests and exact dependency
identities, opens each declared payload root without following links, and pins
the complete closure. It relies on installation's content verification rather
than rescanning archives and expanded files at startup. Process launch additionally
authenticates its bounded descriptor and admits the referenced working directory
and argument paths. This applies to both process extensions and importers.
Manual changes or corruption of other installed content are detected by ordinary
acquisition or installation reuse verification, not proactively at process startup.
Installed code executes with the operator's native authority.
Process and importer exports resolved by the same store may share an already
live lease of the exact named generation. Each launch resolves its own export
role, descriptor bytes and arguments. Releasing the last consumer ends this
reuse. A new process lease rereads the durable metadata and holds its own locks;
an ordinary lease verifies the complete durable content afresh.
For an opt-in cold-start measurement, `RSI_PACKAGE_TIMINGS=1` writes Store
writer-lock wait, manifest and regular content verification, payload verification
(archive plus expanded tree) and process-closure acquisition times to stderr. Records use
the immutable object digest and milliseconds.
