# rsi-workspace-review-api

Authenticated clients list interval summaries for one exact native or observed
conversation in a registered workspace, then read one file diff from that interval.
The owner reauthorizes workspace and source for the actual API origin on every
request, retaining current location admission through the finite worker. Offline
source metadata may be read with Use; a live connection is unnecessary for Local
scratch or durable summaries. Lists return at most
32 summaries and 1 MiB; file pages at most 64 entries and 256 KiB. Diff pages carry
at most 64 KiB of text and explicit byte cursors. IDs and cursors are canonical
strings, not JavaScript numbers. An old runtime epoch or discarded scratch returns
`Expired`; missing, pending and partial evidence are distinct from a complete
empty comparison. Reads confer no execution, rollback, staging or restore authority.
