# rsi-navigation

The ordinary `AttentionFactory` owns a separate version-1 Storage domain of
explicit per-principal reading positions. It combines bounded Session activity
metadata with eight ACP resident observations, never scans recent history, and
does not persist runtime status. Two read slots and one nonqueued writer bound
work; accepted writes survive waiter loss and retirement drains them. Its API
returns pending targets before running, unknown and unread activity. A closed
native durable cut is an unread update, not a guarantee of effect settlement.

A failed backend commit closes this owner's query and mutation admission with an
unknown outcome until Host restart reloads durable truth. Cached metadata cannot
authorize another edit or cursor while its durable revision is uncertain.

This ordinary Host plugin joins durable Session summaries with navigation
metadata. One Storage document contains a global revision and at most 8,192
title/archive records within 8 MiB, including record wrappers. Every read validates
durable bounds; every mutation validates the projected document before writing.
The Session owner remains authoritative for existence and immutable Header data.
Unpublished drafts cannot acquire Host navigation metadata.

Queries scan at most 256 Store rows and return at most 64 matches. Search covers
title, canonical workspace path and exact SessionId without loading transcript
content. Continuation uses the last scanned row, even when there are no matches;
changing metadata, Host generation or query parameters invalidates the cursor.
Workspace grouping resolves exact registered identities without filesystem access
or registration side effects. The [wire contract](../navigation-api/README.md)
owns external fields, bounds and client validation.

Eight non-queued requests and one non-queued writer bound work. The writer owns
global expected-revision CAS and holds the accepted operation through durable
commit and publication if its caller disappears. Retirement closes admission
before draining. Authenticated devices can edit navigation without a configuration
grant. Archive affects navigation visibility only and does not cancel execution.

Reading positions retain at most 4,096 records / 1 MiB. Admission evicts least
recently acknowledged positions until both bounds fit; restart seeds that order
from the durable key order. Eviction may show old activity as unread again, but
cannot acknowledge it for another principal or prevent future acknowledgments.
A failed eviction or write closes admission as an unknown outcome. Ready and
closed external conversations retain unread observations according to their epoch
and sequence, including history loaded from a remote peer.

Navigation wire version 2 separates pinned discovery from ordinary continuation.
The version-1 durable metadata document accepts a missing `pinned` field as false;
new records always write it. At most 64 records may be pinned. Archiving clears
pinning in the same revision CAS. A dedicated pinned query reads every pinned
Header through the read-only Session operation, independent of recent-page depth.
It applies the same title/path/identity, archive and workspace filter, sorts by
creation time then SessionId descending, and reports missing Headers as disabled
entries that may be explicitly unpinned. Other read failures remain errors. A
query never cleans up metadata. Missing-Header records may also be explicitly
cleared by replacing them with default metadata, whether pinned or not, under
the same revision CAS. This cannot create metadata for an unpublished Session.
Ordinary pages exclude pinned entries.

Workspace filters explicitly select all, one registered identity, or unregistered
Headers. No matching row is not proof of exhaustion when a cursor remains.

Pinned Header reads use at most four concurrent reads per bounded pinned query. The
captured metadata revision, missing-Header rows, stable sorting and error semantics
are unchanged; a read failure is not an empty pinned list.
