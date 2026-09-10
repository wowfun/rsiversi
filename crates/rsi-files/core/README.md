# rsi-files

`FilesFactory` supplies the read-only Local service defined by the
[protocol](../protocol/README.md). Each activation has an independent token
namespace and owns its handles; process-wide job/token permits survive waiter
cancellation and generation retirement. Cleanup closes admission, cancels jobs,
withdraws retained tokens and waits for actual blocking work to finish.

On Unix, every root and relative component uses the shared
[native filesystem library](../native-fs/README.md). Content reads check the
opened file type first and accept only regular files. Directory enumeration
never follows entry symlinks. Non-Unix operations return `Unsupported`; this
provider does not substitute path-based reads with weaker confinement.

Run `cargo test -p rsi-files-protocol -p rsi-files` for path/wire, filesystem
replacement, exact paging, cancellation, expiry and capacity invariants. Tests
use canonical temporary roots and no user workspace or credentials.
