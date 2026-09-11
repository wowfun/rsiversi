# rsi-navigation-api

The authenticated navigation API reads durable Session truth and edits only
Host-owned title/archive metadata. It never stops or deletes a Session. Titles
are at most 256 UTF-8 bytes and queries at most 128 bytes. A query scans at most
256 recent rows and returns at most 64 matches. Its continuation names the last
scanned Session, the exact query, archive/workspace filter, Host generation and
metadata revision. Empty pages can have a continuation. New sessions created
ahead of the cursor appear after refresh; a query is not a Store-wide snapshot.

Grouping uses a WorkspaceId derived from the Header's canonical path and an exact
registry lookup. Missing registrations remain unregistered; reads never create
workspaces. Metadata replacement uses an exact global revision and one complete
title/archive record. Clients do not replay writes after unknown outcomes.
