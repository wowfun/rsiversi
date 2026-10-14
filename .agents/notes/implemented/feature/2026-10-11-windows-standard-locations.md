---
name: Windows standard locations from Known Folders
---

## Problem

The standard launcher used HOME and XDG variables on every native host.
Windows does not require them, and Git Bash can give HOME a value unrelated
to the operating system's personal directory. Shared local data should not
be routed to a roaming directory merely because config belongs there.

## Decision

The [standard composition](../../../../crates/rsi/core/README.md) captures Windows
locations through Known Folder APIs. Config uses RoamingAppData; state, cache
and runtime use LocalAppData. Optional personal sources use FOLDERID_Profile.
Explicit HostPaths still override defaults. Missing mandatory locations fail;
an unavailable optional personal directory is omitted. Unix resolution retains
its explicit XDG/HOME policy. The existing `dirs` dependency's Windows backend
calls SHGetKnownFolderPath and does not consult HOME for these folders.

## Alternatives considered

Reading APPDATA, LOCALAPPDATA or HOME would substitute ambient strings for
the selected Windows folders and their redirection policy. Inventing a root
when lookup fails would put durable data somewhere the caller did not select.
Adding raw FFI to the product is unnecessary because the already resolved
platform dependency owns this narrow native API.

## Consequences

The location policy has a native Windows implementation without granting new
filesystem authority or creating directories. Pure tests use fixture roots;
an opt-in native check resolves Known Folders with XDG removed and HOME unset,
empty, unrelated absolute or MSYS-style in isolated child processes. This does not establish private DACLs, Store readiness,
Service transport or application support.
