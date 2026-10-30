---
name: Windows manual terminal authority
---

## Problem

The Session terminal required Linux Bubblewrap even on Windows. Windows has
native pipe execution but no workspace sandbox, so launching a restricted plan
through an unconfined process would misrepresent its authority.

## Decision

Local Windows manual terminals run PowerShell under the current OS account,
separately from Agent sandbox settings. The user explicitly selected this model,
which follows Codex's separate manual process execution path. Terminal status
reports actual access and the UI displays current-user permissions. Process owns
ConPTY and suspended-client Job assignment; Sandbox accepts only an explicitly
unconfined Windows PTY plan. Session selects the shell and non-secret environment.

## Alternatives considered

Keeping manual terminals unavailable until a Windows workspace sandbox exists
would block ordinary interactive work. Silently weakening the Agent's sandbox
or relabeling an unconfined process as WorkspaceWrite would violate its contract.
WSL would add a separate workspace and installation dependency to native work.

## Consequences

Manual Windows commands have the user's filesystem and network access. Agent
permissions remain unchanged. Job ownership controls descendant lifetime but
does not restrict filesystem access. Linux and SSH retain restricted Bash
terminals. Native Windows lifecycle tests establish Windows support; they do
not establish Linux confinement or native macOS behavior.
