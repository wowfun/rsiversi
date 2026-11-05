---
name: Finite host file reads use execution authority and one operation
---

## Problem

The original Files Tools treated the workspace write boundary as a read boundary.
A real uploaded-file conversation attempted `file_read` with the attachment's
absolute path, received `invalid`, then read the same bytes through Bash under
`workspace-write`. This imposed an extra failed model/tool round without protecting
an actual policy boundary. One-page Tools also allocated a retained token and used
separate scope, open, read and cleanup operations.

## Decision

The [Files contract](../../../../crates/rsi-files/protocol/README.md) provides a
finite `read_path` operation. Tool admission still selects the execution lease
and provider; all current Sandbox modes allow host reads. The backend resolves
absolute or cwd-relative native paths, parents and symlinks, then reads one bounded
page using retained handles and version checks. It runs in one blocking job and,
for SSH, one ordinary request. It does not allocate a continuation token.

This adopts the path semantics of DSH's `fs-local/src/fsio.ts`
`resolveLocalTarget` and `tool-fs/src/read-target.ts`: the filesystem backend owns
resolution, including absolute attachments. RSI retains bounded byte pages and
checks object identity around the read. Read paths are data, not execution grants.

## Alternatives considered

An attachment-only exemption would duplicate directory layout knowledge and leave
ordinary absolute paths broken. Prompting the model to use Bash would preserve the
failed native API and spend another provider round. Extending the retained root
API to silently follow links would change human pagination and workspace-scoped
consumers. Those consumers retain their existing contract. The earlier
[execution-location decision](2026-09-29-execution-location-foundation.md) still
owns fixed project-context collectors and their instruction-selection policy.

## Consequences

General reads share the filesystem read visibility already allowed by current
Sandbox modes. `present` and human retained-root readers keep their explicit scope.
Remote paths resolve on the selected target, never the client. SSH helper protocol
version 4 adds the finite operation; an old helper cannot satisfy this contract.
No process enforcement stamp is invented for in-process reads. Windows retains
its current local NTFS handle implementation; native macOS validation is separate.
