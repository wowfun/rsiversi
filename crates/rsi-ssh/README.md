# rsi-ssh

SSH owns the transport and helper mechanics for explicitly authorized target
execution. Its [protocol](protocol/README.md) validates connection coordinates and
pinned public keys. The [client](client/README.md) owns generated OpenSSH input,
independent of user and system SSH configuration.
The [transport](transport/README.md) owns bounded stdio multiplexing, retained
request settlement and credit-based byte streams. It does not execute requests
or feed a watchdog itself.
The [helper](helper/README.md) owns target-side lifecycle verification and feeds
systemd only from fresh heartbeat observations on its exact transport connection.

Target registry, Local trust confirmation, scoped grants and Session admission
belong to the standard product. This family cannot infer permission from an
address, a key fingerprint or a durable workspace. The execution provider tuple
and prepared-plan seal belong to the [Execution family](../rsi-execution/README.md).
