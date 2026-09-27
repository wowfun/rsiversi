# rsi-app-catalog

The library supplies the official native application catalog and immutable built-in
Application Profile metadata. CLI and Desktop consume this same catalog; Service
daemons receive the same metadata for Profile management and reserved plugin IDs.
Factories are constructed against the current explicitly supplied Service
composition after native staging. Local Web invocation inputs remain application
state and do not change Service identity.

Diagnostics cross the core boundary as consuming `ApplicationDiagnostic` trait
objects. The catalog retains no concrete factory bundle after assembly; factories
without an actionable diagnostic slot, including ACP, add no diagnostic owner.
