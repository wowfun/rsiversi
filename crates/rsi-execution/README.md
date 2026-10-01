# rsi-execution

Execution binds a location to one provider generation. Its [protocol](protocol/README.md)
owns bounded location coordinates without filesystem access or authorization.
The [execution capability](core/README.md) pins Sandbox, Process, Duplex, PTY, Files and target
resolution together. Product composition supplies admission; a coordinate never
grants machine access. SSH transport and helper lifecycle belong to rsi-ssh.

The [native adapter](local/README.md) groups explicitly supplied local capabilities
and a fixed program/environment catalog.

Native Sandbox and Process remain independently reusable implementation contracts.
Consumers that select execution locations use an opaque prepared plan from their
exact execution lease rather than choosing those providers independently.
