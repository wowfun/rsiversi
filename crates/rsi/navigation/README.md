# rsi-navigation

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
