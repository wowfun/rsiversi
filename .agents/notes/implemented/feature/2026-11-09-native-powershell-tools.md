---
name: Native PowerShell tools with shared shell execution
---

## Problem

The Windows standard composition exposed Bash syntax and discovered Git for
Windows as its shell prerequisite. Restricted native launches of that MSYS
runtime failed before running the command, including through DSH's launcher.
Native PowerShell 5.1 and 7 successfully executed with the same restricted-token
mechanism. Deriving workspace-review Git from the shell directory also coupled
two independent executables.

## Decision

The [native shell provider](../../../../crates/rsi-shell/native/README.md) owns
both concrete dialects. Windows publishes `pwsh`; Linux publishes `bash`.
The second working dialect now justifies shared preparation, Jobs, output and
settlement mechanics. This supersedes only the single-shell assumption in the
[Agent generation decision](../architecture/2026-08-30-agent-preset-generations.md);
producer and catalog leases keep their separate lifetimes.

The product launcher ports DSH's `packages/shell/pwsh-local/src/resolve.ts`
selection order and `src/index.ts` UTF-8 output preamble. Discovery happens
before composition, with explicit invocation paths passed into ordinary
factories. Store aliases retain their spelling. Git discovery is independent.
The launch identity includes the shell dialect, invocation and selected Git.

## Alternatives considered

Copying the Bash provider into a PowerShell crate would duplicate approval,
cancel, timeout and settlement behavior. Publishing PowerShell under `bash`
would misrepresent its syntax to models and tool hooks. Calling WSL would change
the execution location and filesystem semantics. Keeping Git Bash as the
Windows default would retain a reproduced native restricted-launch failure.

## Consequences

The shell factory freezes scrubbed environment defaults once and shares that
snapshot with catalog generations. It does not probe or canonicalize an
executable at activation; Sandbox owns executable admission at preparation.
PowerShell uses the existing Process/Jobs lifecycle without a new service,
cache, transport or persistent store. Explicit custom compositions can register
either dialect. Native restricted-token guarantees and limitations remain in
the [Sandbox provider contract](../../../../crates/rsi-sandbox/local/README.md).
Hook configurations matching the literal tool name `bash` must select `pwsh`
when they intend to observe native Windows PowerShell commands.
