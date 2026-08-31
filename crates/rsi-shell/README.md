# rsi-shell

This capability family owns shell-specific process production and the
model-facing tools that submit shell work. A concrete shell package owns its
argv, environment policy, output classification, and background Job producer;
the generic Jobs and Process families continue to own job identity/lifecycle
and native process-group mechanics respectively.

Shell implementations register model-visible tools only through one
unpublished [`rsi-tools`](../rsi-tools/README.md) catalog registrar. This keeps
future Bash and PowerShell implementations independently composable without
making either shell part of the generic Jobs, Process, or Tool Runtime cores.
