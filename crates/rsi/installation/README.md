# rsi-installation

This lightweight product library owns the fixed native installation layout,
bounded installation records, PATH receipts, recoverable installation updates
and offline removal policy. It does not compose applications or execute the
installed program during removal. Applications parse invocations; the maintenance owner presents the fixed plan and consent;
Service Host owns process identity, startup coordination and graceful stopping.

## Installation authority

The public executable is `bin/rsi` (`rsi.exe` on Windows). Private installation
files are `libexec/rsi/rsi-uninstall[.exe]`, the platform's `uninstall.sh` or
`uninstall.ps1`, and `libexec/rsi/install.json`. Record format 1 is at most 64 KiB,
rejects unknown fields, and records an installation id, canonical prefix, fixed
artifact digests, phase and exact PATH additions. A record never supplies an
arbitrary deletion path. Publication stages verified bytes in each destination
directory and never replaces an existing installation. Incomplete installs and
removals retain recoverable intent and the uninstaller whenever possible.

Protocol 1 is explicit between scripts and the native tool. Unsupported versions
fail before mutation. Installation and upgrade probes are bounded and share an environment limited to
OS loader, executable search and temporary-directory variables. They do not
inherit homes, product options, credentials or shell injection variables. Existing
unrecorded installations remain unrecorded; an explicitly confirmed `--force`
removal authorizes only fixed installation files, not inferred PATH ownership.
Force never overrides a maintenance lease, pending transaction, unknown live
process, failed stop or path safety check.

## Maintenance transactions

Host startup coordination precedes installation leases. Directory leases use
canonical path order (`bin` before `libexec/rsi` in the fixed layout). Recorded installations use upgrade journal format 2
for the executable, uninstaller, launcher and record. Participants are fixed
roles under the two installation directories. Each rename is same-volume;
journaling provides recoverable consistency, not a cross-directory atomic rename.
Recovery validates every participant before restoring old bytes. Legacy journal
format 1 remains readable and legacy installations retain single-file upgrades.
Recorded installations reject packages without compatible uninstall artifacts.
The installation library dispatches journal versions through one upgrade API;
all formats share the pending filename and empty-file maintenance lease. Protocol
1 requires the exact supported record/transaction capabilities; a capability bump
needs an explicit migration path, not an implicitly accepted downgrade.

Windows upgrade requires private directories; offline removal uses retained
no-follow directory authority and does not change inherited parent ACLs. Empty
coordination locks and namespace guards remain after removal to avoid splitting
concurrent leases. Actual removal can create those coordination anchors in existing
homes even when their payloads are preserved. They are not an installed product.

## Removal and data

Ordinary removal deletes only installation files and verified completed upgrade
artifacts. Homes are per user, not per installation: shared packages and runtime
copies remain and are reported with their sizes. Purge requires explicit consent
after displaying the final homes and their shared impact. Noninteractive consent
uses `--yes`. Dry-run creates no directories, locks, guards or PATH changes.

Purge removes saved credentials, sessions, media, automation, databases, WebView
storage, package/trust state, idle Host artifacts/logs and generated history,
process-output, workspace-review, native application/addon and system-preset
caches. It preserves launch/settings documents, AGENTS.md, skills, agents,
Profiles, custom presets, addon sources, external references and unknown files.
Valid reset/import receipts authorize removal of data backups; names alone do
not. Receipts needed by retained user-authored backups remain. Unverified
potentially sensitive residues prevent reporting complete purge success.
The receipt owner defines explicit reset-config, import-config and state entry
sets consumed by both archive producers and purge; list ordering has no meaning.

Windows prefixes normalize drive and existing component spelling before admission;
normalization never follows a link or junction. Roots cannot redirect through links. Descendant links are never traversed.
Neither an entire prefix nor an entire home is a recursive deletion target.
Pending maintenance must be recovered first. Actual execution repeats preflight
under leases, stops the selected eligible Host gracefully, and holds both home
authorities and package-use exclusion during purge. Partial failure reports
completed and remaining work and preserves retry capability. Size previews stop
after at most 1,024 entries and report a lower bound; they never gate deletion. Removal
streams entries with bounded depth and per-pass work, so a large tree can make
progress on retry. Depth/type failures identify the exact retained path and manual
remediation. Unknown special files are reported in preview and retained.

## PATH receipts

Only confirmed, exact additions are removable. Windows preserves raw registry
text and its original value type, including REG_EXPAND_SZ. POSIX uses a unique
installation marker block and leaves linked or read-only startup files intact.
The intent precedes the PATH write; completion follows it. PATH failure does not
destroy a usable installation. Modified entries, uncertain receipts and PATH
entries serving other programs remain. `--no-path` skips PATH changes.
Removal reports an ambiguous or edited entry that it leaves intact. Linked or
read-only POSIX startup files are preserved with manual PATH-removal guidance.
Windows raw registry writes notify the shell with WM_SETTINGCHANGE (Environment),
using a bounded wait per recipient. Notification failure retains the applied
receipt and explains how to refresh the session. The installation owner confines
the Win32 notification call to a documented native boundary; tests use an isolated
registry key and injected notifier, never the real user PATH.

Run this package's deterministic unit/integration tests for changes to these
boundaries. Tests use isolated prefixes, homes and process identities.
