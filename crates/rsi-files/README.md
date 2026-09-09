# rsi-files

This family owns host filesystem capabilities. The [protocol](protocol/README.md)
owns bounded read contracts and the ordinary [provider](core/README.md) owns
retained handles and blocking work. Its [native filesystem library](native-fs/README.md)
provides directory-handle operations without Session, Workspace registry, trust,
API authentication or model Tool policy. Callers establish their own authority
before opening a root and keep reads relative to that retained handle.

A path or Workspace identity describes a location; neither grants file access.
Reading content does not make it a trusted project instruction. Filesystem
handles, resource accounting and caller authorization have separate owners.
