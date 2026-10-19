# External conversation draft policy

The Web document and TUI retain conversation-owned external drafts for their
current connection or process lifetime. These drafts never enter the native
durable ledger. This document owns their common retention and revision policy.

Each store retains at most 16 records and 1 MiB of UTF-8 draft text. Bound and
pending records are pinned; idle records yield in least-recently-used order,
including their uncertainty fence. Failed admission preserves admitted text
and does not evict unrelated records speculatively.
Recency includes the last editor unbinding. Empty idle records yield only when
the record count exceeds its bound; byte pressure alone does not discard them.

Only an actual admitted text change advances the revision and clears an unknown
send outcome. Cursor movement, reattachment and rejected edits do not. One
attempt belongs to one conversation, revision and store lifetime. Completion
clears text on success or fences it on an unknown outcome only if that revision
is still current. An older completion cannot clear or fence a newer edit.
There is no automatic prompt retry. Retiring the store invalidates old replies.

Web notices change the alert region only when its message or visibility changes.
An edit-capacity notice persists across blocked sends and no-op saves, and clears
when an admitted revision changes, including changes made by another bound pane.
The TUI bounds composed status text to the scene's 8192 UTF-8 bytes on a character
boundary, retaining the leading unknown-outcome warning when truncation is needed.

The adapters deliberately have different editing and concurrency limits:

| Adapter | Editing and pending ownership |
| --- | --- |
| TUI | One active editor, at most 128 KiB UTF-8 per draft and 256 KiB combined editor text/undo allocation. One pending command across the UI owns its submitted text separately (at most 128 KiB); reattachment waits for it. Only draft records count toward the store budget. At most two records can be pinned, so idle eviction always admits a bounded edit. |
| Web | Multiple bound panes share each record, with one pending attempt per conversation. The aggregate store budget includes a pending revision's text separately once it differs from the editable revision. Capacity exhaustion visibly rejects editing or binding. The textarea's `maxlength` is 131072 UTF-16 code units, not a UTF-8 byte cap; the store independently enforces the aggregate UTF-8 budget. |

Pending and edited revisions can coexist; counting their retained text is not
duplicate accounting. Each adapter measures changed text once at admission.
