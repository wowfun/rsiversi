# rsi-app-catalog

The library supplies the official native application catalog and immutable built-in
Application Profile metadata. CLI uses it to select linked applications and
installed executable plugins, including Desktop. Service daemons receive the
same metadata for Profile management and reserved plugin IDs.
Factories are constructed against the current explicitly supplied Service
composition after native staging. Local Web invocation inputs remain application
state and do not change Service identity.
Executable package admission opens and verifies the retained package closure on
a blocking worker. Store contention remains an activation failure with its Busy
diagnostic; it does not run hashing on the async executor.

`rsi.web.assets` admits an internally verified independent Web bundle. Custom
Profiles use the same admission contract as resident Web listeners; native
executable build identity does not select the Web bundle family.

Diagnostics cross the core boundary as consuming `ApplicationDiagnostic` trait
objects. The catalog retains no concrete factory bundle after assembly; factories
without an actionable diagnostic slot, including ACP, add no diagnostic owner.

Product Serve Profiles use `rsi.application.service` and `rsi.application.serve`.
An explicit `--assets` argument selects authenticated Web serving; embedded
`ServeFactory` remains available directly to SDK callers.
SDK integration fixtures compose that factory explicitly with an embedded
Service; the product Serve profile requires a resident Host connection.

The catalog owns public launch help for independently packaged executables.
Those assets live in this package; each executable owns its internal argument help.
