---
name: Preserve native program invocation paths
---

## Problem

Execution canonicalized each configured executable before Sandbox preparation,
and Sandbox canonicalized it again. The first rewrite changed multicall argv[0];
both canonicalization gates rejected a real Microsoft Store PowerShell execution
alias that the native restricted-token launcher successfully executed.
Catalog creation also cloned complete environments, repeated validation and
performed filesystem I/O before the actual preparation boundary.

## Decision

Execution freezes bounded absolute invocation paths and validated environments
without rewriting or probing the program. Its local provider constructor is
synchronous and moves the supplied catalog. Sandbox owns executable file
admission during preparation, on its existing bounded blocking worker.
Workspace coordinate canonicalization remains separate and unchanged.

Following DSH's `pwsh-local/src/resolve.ts`, Windows admission inspects the
directory entry itself for native application execution aliases. Ordinary
symlinks still resolve to a file. Unix admission checks a regular executable
target through metadata while preserving the original invocation spelling.
The launcher owns native execution errors; no path check pins executable bytes.

## Alternatives considered

Skipping Store candidates and selecting Windows PowerShell 5 would mask a
confirmed unsupported validation rule. Adding another process launcher or an
alias-specific bridge would duplicate already working native behavior.
Retaining canonicalization only in Execution would still change multicall
dispatch. Repeating availability checks at capture and preparation cannot
prevent pathname replacement between those checks and process creation.

## Consequences

Missing or nonexecutable programs fail at Sandbox preparation instead of local
catalog capture. Composition can retain its explicit readiness checks before
constructing a shell factory. Native Windows alias execution and Linux alias
dispatch are exercised through the real Execution/Sandbox/Process chain.
Preparation still rejects missing files, directories and nonexecutable Unix
targets; relative, NUL-containing and oversized captured paths remain invalid.
