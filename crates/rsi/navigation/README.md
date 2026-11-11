# rsi-navigation

Workspace Session accounts have a shared Host order, independent of activity,
pin/archive partitions and device-local view ordering. New durable members precede
previously ordered members in descending creation-time/identity order; activity
never moves them. An explicit move writes the selected Workspace's ordered
identity references in the existing metadata document. Accounts are resolved
against the current registration. A recreated directory has a new
registration identity and cannot inherit its predecessor's explicit account.
When an explicit move records the new account, the same commit removes its
members' obsolete account references. A Session is retained in at most one saved
account, without a cleanup task or a second durable identity map.
Unknown or foreign members and anchors are rejected; self and already-positioned moves do not write or advance
the metadata revision. Durable membership remains owned by Session Store, so reads
filter removed identities; writes prune obsolete references for their selected
members without changing unrelated accounts.
The document retains at most 8,192 order references within its existing 8 MiB bound.
An absent order map means there are no explicit moves; empty maps are not stored.
The ordinary Navigation writer owns commit, publication and draining after waiter
loss. No JS registry, duplicated Session payload or separate order journal is used.

Purge cleanup removes only the completed operation's selected Session metadata
and account-order references.
Status is read-only; cleanup is idempotent and preserves unrelated records. Writer
capacity refusal leaves the document unchanged and permits explicit retry. A
pending durable purge cannot authorize navigation removal.

The ordinary `AttentionFactory` owns a separate version-1 Storage domain of
explicit per-principal reading positions. It combines bounded Session activity
metadata with eight ACP resident observations, never scans recent history, and
does not persist runtime status. Two nonqueued request slots shared by reads and
marks, and one nonqueued writer, bound work. A mark reserves its writer before
dispatch; accepted writes survive waiter loss and retirement drains them. Finite
reads start a cancellable owner task on first poll, release their slots on
settled abandonment, and stop on retirement independently of further caller polls.
A returned read reserves a slot, constructs work, and registers with
retirement only when first polled; an unpolled future consumes no request capacity
and cannot keep retirement waiting. Already dispatched backend
blocking reads retain their own lower-level admission until I/O ends. The API
returns pending targets before running, unknown and unread activity. A closed
native durable cut is an unread update, not a guarantee of effect settlement.
Native attention candidates use the actual caller's Session ingress view for
both listing and acknowledgments. Per-principal reading positions never grant
access to another location's Session activity.

Storage failures use the [Domain API projection](../../rsi-storage/domain/README.md).
Unknown commit outcomes close query and mutation admission until Host restart
reloads durable truth; a known pre-commit failure leaves admission available.

This ordinary Host plugin joins durable Session summaries with navigation
metadata. One Storage document contains a global revision and at most 8,192
title/archive records within 8 MiB, including record wrappers. Every read validates
durable bounds; every mutation validates the projected document before writing.
Cold validation seeds exact record-object byte and pin counts. An edit measures
only the old and new entry and projects the revision envelope before publication.
The immutable snapshot and atomic single-record durable format remain unchanged.
Navigation reads the authoritative immutable Header directly from the Session Store. It admits metadata writes using the execution location and protection grant of the actual caller.
Unpublished drafts cannot acquire Host navigation metadata. An archive intent sets the archive flag and clears the pin in one revision. Restoring clears only the archive flag. Both preserve the title and return the caller-visible archives in archive order. Repeated intents leave the revision unchanged. A local operator may clear a stale archive after its Session is gone.

Every endpoint forwards its actual origin. Each finite request captures admitted
location visibility and retains the grant gates until settlement. Store queries
apply this selection before pagination and ordering budgets. Exact summaries
return null for inaccessible identities; pins omit inaccessible identities and rows whose missing Header prevents
authorizing their location or product protection, including Local callers. Metadata edits
admit the selected Header location; configuration grants confer no SSH Use.

Queries select at most 256 indexed activity rows and return at most 64 matches.
An exact registered workspace uses the coordinate/activity index. Search covers
title, canonical workspace path and exact SessionId without loading transcript
content. Continuation uses the last scanned row, even when there are no matches;
changing metadata, Host generation or query parameters invalidates the cursor.
Activity changes do not invalidate continuation. The page carries the newest
activity key from the same Store snapshot so clients can offer an explicit refresh.
It is absent when the newest indexed identity cannot be authorized, even on a
nonempty page; omission never exposes that hidden identity.
Workspace grouping resolves exact registered identities without filesystem access
or registration side effects. Each page, pin list or exact-summary request shares
one lookup per distinct coordinate, with at most four lookups in flight. Results
retain input order; an ordinary page stops at its match limit without draining
the remaining lookup queue.
The [wire contract](../navigation-api/README.md)
owns external fields, bounds and client validation.

Eight non-queued requests and one non-queued writer bound work.
Finite query, pin, order-seed and summary reads use the caller ownership and
first-poll retirement contract above. The writer owns
global expected-revision CAS and holds the accepted operation through durable
commit and publication if its caller disappears. Retirement closes admission
before draining. Authenticated devices can edit navigation without a configuration
grant. Archive affects navigation visibility only and does not cancel execution.

Reading positions retain at most 4,096 records / 1 MiB. Admission evicts least
recently acknowledged positions until both bounds fit; restart seeds that order
from the durable key order. Eviction may show old activity as unread again, but
cannot acknowledge it for another principal or prevent future acknowledgments.
Reading positions cache their entry sizes and use the
[Storage record-object accounting](../../rsi-storage/core/README.md) for projected
bounds. An acknowledgment measures only its new position; it neither clones nor
encodes unrelated positions. Eviction walks recency only until both bounds fit.
Eviction and the requested acknowledgment are separate record commits. Confirmed
evictions remain effective even if a later deletion or acknowledgment fails;
each confirmed deletion is also immediately reflected in the cache. A known
failure leaves the requested position unacknowledged and allows a later explicit
retry. Unknown outcomes fence the retained Storage generation. Observing sources
is rejected before I/O when that generation is already unavailable. Ready and
closed external conversations retain unread observations according to their epoch
and sequence, including history loaded from a remote peer.

The version-1 metadata document retains the pinned field and bounds owned by the
[wire contract](../navigation-api/README.md). Missing activity may remain explicit when the Header still authorizes the read;
queries never clean up metadata. Missing-Header records may also be explicitly
cleared by replacing them with default metadata, whether pinned or not, under
the same revision CAS. This cannot create metadata for an unpublished Session.
Ordinary pages exclude pinned entries.

Workspace filters explicitly select all, one registered identity, or unregistered
Headers. No matching row is not proof of exhaustion when a cursor remains.

Pinned summaries use one bounded Store snapshot for at most 64 exact identities.
Ordinary and pinned listing authorize immutable Headers and do not decode transcript bodies. Available pins follow the order in which the user pinned them, newest first. Repeat pin requests keep their position. Records written before pin order was retained follow explicit pins in activity and identity order; absent activity retains explicit missing entries only when its immutable Header
authorizes the caller. Metadata edits continue to check durable existence separately.

Cache availability follows [Storage generation health and recovery](../../rsi-storage/core/README.md).

Manual membership reads the Store's complete bounded identity snapshot and joins
one immutable metadata revision. A seed includes both pin and archive partitions,
so filtering a view cannot silently remove a device's saved members. Exact summary
reads preserve the requested order and metadata revision; missing rows stay null.

Protected Session scope is read from its immutable Header before navigation,
pinned summaries, complete manual membership or exact summaries are published.
The actual caller must hold the product scope's View grant. Unprotected Sessions require no revocation lease. Unknown scopes are
hidden; revocation ends the finite read lease. Candidate indexes remain bounded
and no transcript is read. This adds bounded Header authorization reads to the
metadata query path; identities and stored titles alone never authorize a row.

Public scan cursors use an opaque process-local token instead of publishing a
skipped protected Session identity. The owner retains at most 128 cursor cuts,
with at most eight per principal and a five-minute lifetime. A successful
continuation replaces its predecessor; completion releases it. New scans may
evict only that principal's oldest token. A full book rejects another principal
with Capacity rather than evicting somebody else's continuation. Expired tokens
require an explicit refresh. Cuts bind the caller, query and metadata revision.
A token locates a scan cut and grants no source
access; every new page applies current Header scope policy again.

Header lookup failure propagates as unavailable; an absent Header or denied
protection scope is omitted. Storage failure cannot be presented as a hidden row.

Completed Session purge operations have an independent, idempotent navigation
cleanup receipt. Cleanup resolves the original completed Store selection and
removes only those metadata records and account-order references, preserving all
unselected records. Failure
leaves canonical purge complete and is retried through the same operation status;
absence of this owner is reported as unavailable rather than successful cleanup.

Native directory observers may sample the current metadata revision without reading membership. Rows and pin or archive state are always obtained through the authenticated read.
