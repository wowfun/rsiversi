# rsi-spill

Runtime-independent storage contracts for the [Spill family](../README.md).
Scope identifiers are hashes of caller-selected namespaces and identities.
Locators contain only a scope and an unpredictable object identity. Constructors
and deserializers bound these external values; in-process consumers trust the
validated types. Capture sinks carry one scope without exposing Session types.

`push` never waits for disk or queue capacity. `write` may await bounded queue
admission and is intended for an already retained text producer. `finish` waits
for completed publication; abandoning or dropping the last capture before that
publication prevents it. A store fences a scope synchronously before asynchronous
cleanup. Readers expose bounded raw pages, independently of write authority.

Pages use shared immutable bytes; remote clones and slices retain the original
receive lease. Readers reject offsets beyond EOF. Adapters check exact identity,
offset, byte length, the 64 MiB object bound and nonempty progress before EOF.
Transport failures retain their API classification. The authenticated read API
confers no execution or Session authority.
