---
name: Authenticated browser windows share one product owner
---

## Problem

A single operator can open the workbench from several browser profiles or
devices. Giving each login its own application identity hid unpublished drafts
and prevented another window from answering a pending approval.

## Decision

HTTP authenticates each cookie and checks the credential expected by the
client. It then dispatches browser work under the deployment's browser owner.
The product uses its existing managed browser identity so Workspace, Session
and configuration grants agree across listeners. The DSH bridge uses the
released connection service's browser Peer and its ordinary Remote Event
waterfall. The first answer settles the native interaction.

Long-lived plugin activity cannot borrow a physical HTTP request. Native
composition supplies a separate callback guard under the same product owner,
with the application process as its lifetime. This follows the DSH factory's
Cordis ownership: closing one browser does not dispose a server-side Agent.
Credential revocation still closes that credential's requests; it does not
silently terminate work owned by the shared application. The callback uses
device-scoped ingress, never local administration, and process retirement
invalidates it together with its retained resources.
The bridge drains Cordis before retiring its native activity scope, so a plugin's
disposer can still stop the work it owns. That scope is not a concurrently
disposed sibling effect of the plugins which depend on it.

## Alternatives considered

Binding a process to the first browser credential makes access depend on
launch order. Constructing an application Peer per device duplicates DSH's
connection lifecycle and splits the same operator's work. Treating a browser
as a local transport would also grant operations reserved for a local caller.

## Consequences

An authenticated browser can exercise the owner's installed plugin capabilities
and see unfinished drafts from other logged-in windows. Revoking a credential
closes requests and streams authenticated with that credential. Launch tickets
issued by the same Host reuse its managed credential, so this also applies
across windows that used those tickets. Separately registered credentials can
be revoked individually. Bearer API clients retain their registered identities.
The [device registry](../../../../crates/rsi-api/auth/README.md) defines the
lifetime of the managed slot when a local administrator deletes the device.
