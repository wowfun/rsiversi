# rsi-execution-local

This library groups explicitly supplied native Sandbox, Process, Duplex, PTY and
Files capabilities into one Execution backend. It does not discover or replace
providers. Composition supplies an exact finite program catalog; each program
has an absolute executable path and a complete child environment. Missing
selectors fail closed. Distinct selectors may resolve to the same executable with
different environments; the opaque resolved program keeps that selection intact. No ambient PATH, HOME or environment lookup occurs.
Preparation freezes that program's environment into the opaque plan. Spawn
rejects environment substitution before backend I/O.

Path canonicalization uses this machine only. The adapter cannot be constructed
as an SSH backend: `provider()` always issues Local coordinates with zero target
revision and connection epoch. Native process owners retain the
whole provider tuple through settlement. Start admission settles at verified
publication; a running terminal cannot hold a completed start grant open. Existing native provider retirement
can still reject a pinned operation; the adapter never substitutes a new provider.

Tests exercise the public Execution lease with the real Linux native providers,
explicit temporary workspaces and isolated child environments. Scripted core tests
cover lost replies and admission revocation separately from native process evidence.
