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

## Alternatives considered

A path precheck followed by recursive mkdir does not preserve the checked parent.
Putting product trust, manifests or Workspace policy in the mechanics library
would reverse ownership. Rolling back a partially created directory chain can
remove directories another actor already adopted.

## Consequences

I/O failure may leave earlier created parents. Public tests cover creation,
existing permissions, traversal rejection before writes, linked parents and
retained-handle behavior after root replacement. The implementation is Unix;
Linux tests do not establish native macOS behavior.
