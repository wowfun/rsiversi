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
Purge uses the same per-object traversal bounds as staging cleanup and GC, so
every admitted object can be removed in one pass without a smaller store-wide cap.

This library owns bounded, non-executing package admission and immutable storage
below the selected state home. Applications supply downloads and source recipes;
installing a package never runs a compiler, lifecycle script or executable.
Profiles select activation. The resolution lock records installed names, exact
manifest digests and dependency closures; it contains no enabled flag.

A format-2 manifest declares its target, every file's relative path, length and SHA-256,
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

Installed objects remain immutable. A consumer leases its complete resolved
closure before exposing paths. Updating the resolution lock does not mutate
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
require exclusive ownership. Acquisition pins objects before releasing that
coordinator. Installation collects unselected, unleased generations and
abandoned stages before capacity accounting, and syncs every staged directory
from leaves through the object root before publication. Recovery enumeration
allows the 256 committed objects plus at most 128 interrupted staging names.
Installation also removes abandoned `stage-<id>` trees under the same writer
lease before accounting capacity, including a crash before manifest publication.
These unpublished names never carry consumer leases; selected immutable objects
remain subject to the ordinary GC rules.

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

Source producers may use `install_checked` to revalidate their captured inputs
and cancellation immediately before the atomic resolution write. This callback
runs under the package writer lease after complete object staging. Rejection
leaves the previous selection intact; independently accepted trust receipts remain.
Staged immutable objects
are reclaimed before returning, retaining any selected or leased generation.
If resolution publication reports a durability error after rename, cleanup reads
the actual resolution again before deciding which objects are unselected. The callback is an in-process producer fence,
not package-provided executable code.
