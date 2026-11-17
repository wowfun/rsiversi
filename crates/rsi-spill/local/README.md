# rsi-spill-local

Worker shutdown explicitly unlocks the writer lease before closing its descriptor,
so a duplicated or fork-inherited descriptor cannot extend writer ownership.

The local [Spill](../README.md) provider uses an explicitly selected private
directory. One worker holds its writer lease until every admitted filesystem
operation settles. Generated scope directories contain exclusive `.part` files
until successful publication as `.log`; readers never open partial files.

An 8 MiB semaphore accounts queued and in-flight chunks. Process writers use
nonblocking admission, while already retained text can await that same bound.
EOF waits at most 250 ms; timeout abandons publication without abandoning worker
ownership. Reads and cleanup-status queries wait at most five seconds; timeout
does not release the worker's native lease or preempt a filesystem operation.
Cleanup and reads share the worker. One scoped cleanup or status cursor advances
in slices, with capture commands serviced between slices. A competing cleanup
request refuses with Capacity; a status query for the scope being cleaned reports
pending. The background retention cursor pauses during scoped traversal.
Cancelled status queries release their cursor; accepted removal continues even
when its caller stops waiting.
Startup admits the store after acquiring its lease;
the sweep then advances through retained directory cursors. Each scan slice
visits at most 32 entries and yields after 2 ms of completed filesystem work,
with up to 64 queued commands serviced before the next slice. A single native
filesystem call cannot be preempted. No scan truncates the retained namespace.
Reads and new captures refuse persisted purge markers before a sweep reaches
them; startup partial cleanup skips captures started in this generation.
The worker checks its stop flag independently of queue
capacity and releases its lease before acknowledging shutdown.

Capture admission is memory-only until the first nonempty chunk. Empty streams
return no locator and create no scope directory, partial or completed object.
The default free-space floor is 256 MiB, the object limit is 64 MiB and the age
limit is 30 days. There are no aggregate byte or file-count limits. Available
space is sampled on capture admission and at most 1 MiB of writes apart; the
sample includes queued/in-flight bytes. Failed or oversized captures retain no
complete locator. Startup discards this provider's abandoned partial files.
An hourly scan expires only generated complete regular files strictly older
than the cutoff; `cleanup_period_days: 0` disables age expiry only.

Scope cleanup first fences publication, then removes generated complete/partial
files. A private `.purged` marker records incomplete cleanup for startup retry. It is
removed when generated objects are absent; a remaining marker is not clean.
The in-process fence survives cleanup, and canonical Session retirement prevents
new producers after restart. Unknown children are preserved. This is cache cleanup, independent of canonical Store
deletion. No filesystem operation follows an output locator as an arbitrary path.
