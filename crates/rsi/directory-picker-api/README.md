# rsi-directory-picker-api

The standard product's directory picker uses authenticated version-2 `status`,
`list` and `create` operations. Its ordinary client plugin is shared by Worker
and native GUI transports. Status reports platform support and grant availability;
it discloses no filesystem paths. Every request explicitly selects an execution location. Local browse and creation
require ConfigurationAccess; SSH browse and creation require the caller's current
Use grant for that exact target. Status tests admission without connecting or
disclosing target paths. A known workspace path can still be registered independently.

List accepts an optional absolute UTF-8 path in the selected location, at most 16 KiB. Omission selects
the Local home captured by composition, or the target account home resolved by the helper. The result names physical path, physical home,
path-component breadcrumbs, directory entries and explicit truncation and skipped
non-UTF-8 indicators. It is one sorted window, not a paginated catalog: at most
1,000 entries and 2 MiB encoded, ordered by UTF-8 name bytes. Hidden entries remain
marked so display filtering does not imply an exhaustive visible directory.
Each entry has a physical absolute target path, name and symlink/hidden flags.
Broken/cyclic links and nondirectories are not selectable. A user may enter a
deeper path directly when a list is truncated.

Create accepts a parent and a single nonempty name fragment (at most 255 bytes),
rejecting separators, NUL, dot and dot-dot, and rejects existing targets. It neither
recursively creates parents nor overwrites. The result is a physical directory
path suitable for Workspace registration. Unknown creation outcomes require
explicit readback and must not be retried automatically.

Non-Unix Hosts expose Unsupported through the same capability, allowing manual
path registration without failing required client activation. Clients validate
closed replies, input/output bounds and request identity before presentation.
