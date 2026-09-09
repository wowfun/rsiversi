---
name: Private root creation through retained directory handles
---

## Problem

Creating a product storage root with path-based recursive mkdir before opening a
no-follow handle can already follow a replaced or linked parent. Directory
creation needs the same authority boundary as later file access.

## Decision

The Unix native filesystem library acquires absolute roots component by
component, optionally creating missing directories relative to the retained
parent. It validates the complete path before mutation and opens every created
or existing component without following links. New directories request 0700;
existing permissions remain unchanged. Products still choose and authorize the
root and own filesystem quotas and lifetime.

An explicit path helper resolves only the first component below `/` when a
caller authorizes an OS-owned alias. It leaves the suffix untouched and grants
no handle authority; subsequent acquisition remains no-follow. Agent preset
roots consume this shared mechanism while retaining their own root policy and
creation permissions. The ordinary root-acquisition APIs never enable alias
resolution implicitly. Full preflight rejects embedded NUL before any parent
creation, rather than relying on a later operating-system open to reject it.

## Alternatives considered

A path precheck followed by recursive mkdir does not preserve the checked parent.
Putting product trust, manifests or Workspace policy in the mechanics library
would reverse ownership. Rolling back a partially created directory chain can
remove directories another actor already adopted.

## Consequences

I/O failure may leave earlier created parents. Public tests cover creation,
existing permissions, traversal rejection before writes, linked parents and
retained-handle behavior after root replacement. The implementation is Unix;
additional probes cover NUL rejection before writes and explicit alias
resolution without following deeper links. Linux tests do not establish native
macOS behavior.
