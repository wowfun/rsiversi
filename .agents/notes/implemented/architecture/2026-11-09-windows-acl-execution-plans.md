---
name: Native Windows ACL execution plans
---

## Problem

Windows Process supports token-based creation, but ordinary restricted Sandbox
requests previously failed before tool execution. Pretending a write-restricted
Windows token provides a read-only mount view would also misstate enforcement.
Execution lifetime wrappers must not hide the token from the native launcher.

## Decision

The local Sandbox plugin owns Windows token creation, path grants and private
scratch. Process consumes its exact borrowed token, while Execution preserves
the native resource owner when retaining its provider and admission pin.
The [Sandbox contract](../../../../crates/rsi-sandbox/core/README.md) owns the
durable evidence combinations and their actual boundaries.

The implementation follows DSH's Windows ACL package: deterministic workspace
SIDs, standing workspace grants, a Low-integrity write-restricted token, and
workspace locks compatible with its file location and byte-zero range. Existing
matching grants skip recursive security-descriptor propagation. Fresh private
directories have exclusive owners and need no persistent lock files.

Both restricted modes receive private writable scratch, including read-only.
PowerShell 5 and 7 entered ConstrainedLanguage without a writable temporary
policy-probe directory in the native comparison. Adding only the private scratch
SID restored FullLanguage while workspace writes remained denied. PowerShell's
GetAppLockerPolicy source explains the temporary-file dependency. This avoids
injecting language-policy overrides or granting workspace writes to a reader.

## Alternatives considered

A helper process would duplicate the native launch and settlement path. A
global token or temporary directory would share write capabilities across
independent workspaces or executions. Copying DSH's read-only token unchanged
would retain the observed PowerShell language restriction. Removing Everyone
or the logon SID instead breaks CLR, crypto initialization or process startup
on the exercised host; stronger mount-style claims are therefore rejected.

## Consequences

The backend is native Windows pipe execution with host networking. Existing
Low-integrity objects admitting ambient SIDs can remain writable; the durable
filesystem variants state this instead of claiming Linux mount enforcement.
Restricted ConPTY and isolated source-reader processes remain unsupported.

Workspace preparation may install a standing capability ACE before approval;
no child starts at that point. A Modify-only directory owned by the caller may
receive that owner's missing WRITE_OWNER right to install its Low label. Other
ownership is not changed. Plan release owns temporary cleanup through completion;
normal async runtimes move recursive removal to blocking work. Abrupt process
death can still leave scratch directories and standing workspace ACLs.

Native ACL tests are opt-in and edit only newly created fixture directories.
Default tests cover evidence decoding, resource retention and typed error
propagation without modifying host ACL policy. Windows behavior and Linux
regressions do not establish a 512 MB device memory budget or ARM support.
