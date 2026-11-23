# rsi-navigation

Workspace membership is an explicit relationship between a registration and a
Session. An execution directory identifies where the Agent operates. Sharing
that directory does not enroll a plugin-created Agent in a chat workspace.
Attaching a Session checks its stored execution coordinates and the caller's
access, then prepends its ID to the registration's shared account. Repeated
attachment leaves its order and the durable revision intact.

The account stores one ordered list of Session IDs per registration. Position is
independent of activity, pin and archive state, and device-local ordering.
Moves require both the member and its anchor to belong to that registration.
Self moves and already-positioned moves preserve the metadata revision. A
replacement registration at the same directory starts empty. Explicit attachment
removes the selected Session from a former registration in the same transaction.
Discovery intersects the account with the caller's visible Store identities.
Unattached identities remain available through authorized global and exact reads.
A single coordinate can therefore contribute both registered and unattached rows.

The Navigation writer commits membership and order changes before publication.
It retains at most 8,192 references within the existing 8 MiB document bound.
Empty account lists take no durable storage.

Purge cleanup removes only the completed operation's selected Session metadata
and account-order references.
Status is read-only; cleanup is idempotent and preserves unrelated records. Writer
capacity refusal leaves the document unchanged and permits explicit retry. A
pending durable purge cannot authorize navigation removal.

Reading state belongs to the live Host. Devices authorized to see the same
conversation share its read position and explicit unread flag. A new native
Turn completion advances the reminder even when no client is observing.
Opening historical Sessions after restart starts with their history read.
Reading changes write no storage. Acknowledging a rendered cut preserves an
explicit unread flag. Marking it read clears that flag and advances only
through the observed cut. Both use the caller's current Session visibility.
Reading a zero cut or clearing its last manual flag retains no per-conversation
entry unless the Host has a completion or a read position to remember.

Pending targets precede running, unknown and unread idle activity. Two
nonqueued request slots bound concurrent attention operations. A finite read
starts on first poll and retires on abandonment or shutdown. An accepted mark
settles even if its caller disconnects. Neither operation hydrates a Session. The live feed coalesces reading changes and
rechecks visibility before publishing each replacement. Cold reminders are
resolved in batches of 64. The page retains the first authorized entries in
priority and identity order across resident and cold Sessions. It also samples the
other activity owners for changes in running work, access or human interaction.

Storage failures use the [Domain API projection](../../rsi-storage/domain/README.md).
Unknown commit outcomes close query and mutation admission until Host restart
reloads durable truth; a known pre-commit failure leaves admission available.

This ordinary Host plugin joins durable Session summaries with navigation
metadata. Domain schema 2 stores a global revision, a `metadata` table and an
`orders` table. It requires atomic batches (SQLite or single JSON); per-record
JSON is refused before activation. At most 8,192 title/archive records and 8,192
order references fit within the same logical 8 MiB document bound, including
record wrappers. Cold loading validates durable bounds once; every mutation
validates its projected accounting before writing.
Cold validation seeds exact record-object byte and pin counts. An edit measures
only the old and new entry and projects the revision envelope before publication.
Only affected records and the revision are encoded and committed in one batch.
Publication follows durability. Readers retain immutable snapshots; a concurrent
reader can require a copy of the in-memory index at publication, but an uncontended
edit does not clone that index. Failed writes publish no part of the change.
Navigation reads the authoritative immutable Header directly from the Session Store. It admits metadata writes using the execution location and protection grant of the actual caller.
Unpublished drafts cannot acquire Host navigation metadata. The Host owner may archive or restore. Ordinary device metadata edits preserve archive membership. Archiving checks the Session and its owned activity under the navigation writer. An idle archive sets the archive flag and clears the pin in one revision. When the owner confirms stopping active work, archive membership is durable before cancellation begins. A stop failure leaves the archive in place. Restoring clears only the archive flag. Both preserve the latest title. The extension receives caller-visible archives in archive order. A single-Session Web action receives only that Session's resulting metadata. Repeated intents leave the revision unchanged. A local operator may clear a stale archive after its Session is gone.

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
retain input order. Coordinate selection and lookup preparation advance with
the four in-flight slots. An ordinary page stops at its match limit.
The [wire contract](../navigation-api/README.md)
owns external fields, bounds and client validation.

Eight non-queued requests and one non-queued writer bound work.
Finite query, pin, order-seed and summary reads use the caller ownership and
first-poll retirement contract above. The writer owns
global expected-revision CAS and holds the accepted operation through durable
commit and publication if its caller disappears. Retirement closes admission
before draining. Authenticated devices can edit navigation without a configuration
grant. Archiving hides the Session and suspends future scheduled deliveries to
it. The executor checks this committed membership before each new model attempt,
including retries, compaction, manual sends and background wakeups. Subagents
inherit the archive state of their ancestors. Independent conversation forks
retain their own archive state. Already admitted work keeps its identity.
Restoring the Session permits subsequent attempts and makes its outstanding
reminder eligible again. Profiles without navigation admit requests normally.

The metadata records retain the pinned field and bounds owned by the
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

The owner-local persistence gate covers both navigation metadata and account
attention. It records serialization schemas and exact raw
Domain mutation bytes from `tests::persistence::capture_current_writer`. Capture
requires a new `RSI_NAVIGATION_CAPTURE` directory; it never replaces an existing
corpus. The retained gate requires current-version writer evidence, validates its
actual codecs, and opens older versions through the real Domain to verify explicit
refusal without writes. No old schema is inferred from the current structs.
