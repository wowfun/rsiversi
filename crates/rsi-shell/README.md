# rsi-shell

This capability family owns shell-specific process production and the
model-facing tools that submit shell work. A concrete shell package owns its
argv, environment policy, output classification, and background Job producer;
the generic Jobs and Process families continue to own job identity/lifecycle
and native process-group mechanics respectively.

The [native provider](native/README.md) composes Bash or PowerShell through an
explicit shell kind, executable and environment. Each kind has its own tool and
Job producer identity while sharing preparation, approval, output and settlement
mechanics. Shell argv and encoding stay in the provider's dialect module; no
shell-specific behavior enters Jobs, Process or Tool Runtime.

Shell implementations register model-visible tools only through one
unpublished [`rsi-tools`](../rsi-tools/README.md) catalog registrar. Both native
kinds remain independently composable through ordinary factory instances.
