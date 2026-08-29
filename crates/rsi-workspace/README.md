# rsi-workspace

`rsi-workspace` is an ordinary plugin for durable host-local workspace
registrations. It requires the non-session domain facility, stores canonical
physical absolute paths and stable order, and provides a Local registry. Each
workspace is one bounded domain record carrying its immutable insertion order;
mutations never rewrite unrelated registrations.

Workspace state is not Agent context, a sandbox grant, or directory ownership.
Deleting a registration removes only domain records; user directories, files,
Sessions, and Agent facts are never removed. Missing directories are reported
by status and do not mutate the registry.
