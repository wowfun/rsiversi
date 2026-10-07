# Linux CI user manager lifecycle

`user_manager.py` prepares the Linux user manager required by the Browser, Web
and SSH acceptance jobs. It records preexisting linger and manager state before
the first mutation. An `always()` step restores only resources created by that
job, including after partial setup failure; preexisting services remain running.
The state file lives in the runner's private temporary directory.

Run deterministic lifecycle checks with
`python3 fixtures/tools/python-tests.py fixtures/rsi/browser-runtime`.
They mock system commands and use temporary state, without changing host policy.
The native-gate script check requires Bash and ripgrep; it executes the actual
workflow script with isolated cargo and privilege-command substitutes.
Real manager readiness and process confinement remain the native Browser gate.
