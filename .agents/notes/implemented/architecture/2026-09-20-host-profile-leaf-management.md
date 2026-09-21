---
name: Reviewed single-leaf Host Profile management
---

## Problem

The existing source editor compiles and resolves programs but deliberately does
not prepare plugins. Configuration grants cover closed Settings namespaces;
they cannot authorize executable Host Profile source changes. Exposing the raw
editor would disclose source secrets and permit unrelated tree mutations.

## Decision

Own reviewed single-leaf edits in the product. Select an existing writable user
Host root and leaf, append a strict root override, prepare the proposed enabled
graph and return only redacted effective changes and a review digest. Keep the
existing directory identity, source bytes and dependency checks at publication.
Use a Profile-owned literal `config_json` form for generated overrides: TOML
cannot encode nested nulls or every exact JSON number, and converting these to
Rhai would introduce expression semantics and numeric rounding.
Require separate explicit Local grants scoped to principal, source root,
Profile, leaf and operation. UI and model tools share that owner; neither a
configuration grant nor an Agent's tool invocation supplies this grant.
This narrowly extends the source-only management boundary in the
[GUI decision](../../implemented/architecture/2026-09-11-plugin-composed-gui.md):
explicit leaf grants authorize this reviewed source operation. The existing
closed Settings namespace policy remains authoritative for Settings writes;
no Device gains general Profile authoring or addon Settings authority.

Retain admitted work and exact receipts independently of response waiters.
The review digest includes the native source directory identity, so rebuilding
the in-memory proposal cannot accept an identically populated replacement directory.
Saving is separate from observed application, restart requirements and failed
convergence. Disabling a parent group cannot be silently reversed by a leaf
enable operation. Existing Session generations remain pinned.

## Alternatives considered

Sending complete source programs over the configuration API conflates arbitrary
composition authority with leaf editing and exposes secrets. Reusing Settings
grants silently expands existing device authority. Claiming preparation from
pure compilation admits invalid plugin configurations. Restarting residents
would violate the existing immutable Agent generation contract.

## Consequences

Source preparation, grants and receipt ownership are shared by human clients and
Agent tools. Deterministic owner tests cover imported overrides, disabled parents,
source/directory conflicts, Device and Agent isolation, dropped waiters, scope
revocation and restart observations without replacing resident generations.
Real PTY, Chromium, Firefox and Linux WebKitGTK paths exercise grant admission,
reviewed publication and exact receipt recovery. Raw source and configurations
remain hidden from previews.

Preparation executes trusted plugin code and can fail or block. Admission is
bounded and owned work is retained through completion. A saved source may require
restart or fail convergence; publication never claims runtime success. Source
receipts last for the Host epoch, while grants persist in their own Storage domain.
