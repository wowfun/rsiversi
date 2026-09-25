---
name: Observed plugin diagnostics and typed settings controls
---

## Problem

Desired Profile nodes alone cannot establish running plugin state. The existing
`ProfileStatus::observed` observations and Configuration grant boundary provide
the required information and authority. Generic JSON editing obscures ordinary
Settings fields whose owning schema already defines typed values and constraints.

## Decision

Diagnostics distinguish current Host state, pure current preset previews and
the exact resident Session generation. A redacted manifest is captured with each
generation and retained by its pin. A dedicated residency peek cannot build a
replacement merely to inspect it. A cold Session explicitly has no resident
manifest; its later normal resume may select current preset sources. Session
targets retain the product's Header correlation and Configuration grant checks.
Source classes and fixed reason guidance expose no dependency keys or raw errors.

Local Inspector keeps its Local-only API policy and separately projects observed
Profile instances. A new finite Configuration read exposes only flat plugin
identities, desired enablement and closed lifecycle categories. Its handler
holds the actual ConfigurationAccess lease until the read finishes. It exposes
neither Runtime topology nor raw errors, source paths, configurations or values.
Desired-tree, Profile-status and provider desired/applied revisions remain
separate; no client infers an atomic snapshot or calls a configured plugin active.
Profile-status revision advances for all observation changes, including watcher
and lifecycle transitions and preflight errors. A command-worker-owned completed
attempt records a separate sequence, origin and closed outcome/failure categories.
Busy admission and polling do not overwrite it. Local Inspector retains bounded
Pending reasons; remote version 3 pages expose only the closed attempt categories.

An ordinary shared workbench feature owns read/refresh and bounded page state.
TUI /plugins and GUI Settings → Plugins consume it. GUI Settings forms follow the
owning typed schema: object fields, strings, numbers, booleans and enum choices;
complex structures retain an explicit JSON editor. Existing version CAS,
validation, apply timing and last-good values stay with Settings. Credentials
remain a separate bounded secret operation.

## Alternatives considered

Making inspector.* remotely accessible would transfer topology and unrelated
operator authority. Polling arbitrary factories or calling prepare to discover
status would execute work merely to inspect it. Reimplementing grant checks in
JavaScript would not establish authority. Hiding distinct revisions would imply
convergence which the existing Profile and provider contracts do not guarantee.

## Consequences

Pending, active, failed, disabled, restart-required and unavailable states use
source observations. Ungranted/revoked callers fail before source access, and
revocation drains a held read lease. Oversized or malformed pages fail explicitly.
Both clients support read/refresh with focus and draft preservation. Scalar and
enum controls round-trip through real Settings validation/version checks; secret
material never appears in standard UI model values or diagnostic output.

### Trade-offs

Desired and observed snapshots can change independently. The workbench must show
both revisions and reject mixed-version pagination. A revoked grant must fence
new status reads and drain admitted reads. Rendering JSON numbers or multiline
strings through browser controls can change values before the owner sees them;
unsupported or inexact representations must preserve the original JSON editor.
