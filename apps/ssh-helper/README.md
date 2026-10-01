# rsi-ssh-helper-app

This thin Linux entry delegates argument validation, immutable cache installation,
user-systemd launch and target execution to the reusable
[helper library](../../crates/rsi-ssh/helper/README.md). Its four arguments are the
closed mode, Service cache namespace, connection epoch and artifact SHA-256.
The standard product's explicitly authorized SSH connection owner supplies them.

Binary stdin/stdout belongs exclusively to the helper mux after initialization.
Diagnostics go to stderr and do not include policy environment or command contents.
Native prerequisites fail closed. The distribution owner supplies a same-CPU Linux
musl image and binds its SHA-256 to the receipt used by the connection owner.

The same immutable image exposes fixed apply-patch, project-context and directory-picker entries
before mux argument dispatch. The application links their owning libraries; SSH
transport does not interpret Agent state. The project-context entry uses the
[workspace source contract](../../crates/rsi-agent/workspace-context/README.md),
with confined cwd and no Service user-source configuration.

The Linux SSH CI job runs the opt-in native lifecycle and actual OpenSSH tests
with an isolated generated identity and host key. It requires user systemd,
cgroup v2, enabled service watchdogs, Bubblewrap, openat2 and the same-CPU musl
helper. The fixture uses the installed sshd's normal subprocess paths by default;
`RSI_TEST_SSH_SESSION` and `RSI_TEST_SSH_AUTH` select explicit subprocess paths
only when using an extracted newer OpenSSH package. No developer SSH settings or
keys are read. The aggregate CI contract requires this job.
