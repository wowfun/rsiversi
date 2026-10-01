# Workspace access fixture

The deterministic resolver implements the public Execution location and admission
seams for Workspace registry and API tests. It lets those consumers vary metadata
visibility and authority without a real remote host. It proves controller access
checks, not SSH connectivity or native filesystem enforcement.

The `rsi-workspace` and `rsi-workspace-api` integration tests include this module
by path. Run their `workspace` test targets; no standalone Cargo workspace or
lockfile is needed here.
