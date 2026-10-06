# rsi-automation-api

The version-one Automation operations negotiate through the ordinary API client.
The Automation owner authorizes actual transport origins, never request fields.
Identifiers and cursors are canonical decimal strings. Pages contain at most 50
summaries, an exclusive scan cursor and a fixed watermark; filtering cannot stall
progress. Get returns frozen check evidence separately from exploration state and
claims. PNG evidence is a bounded, grant-authorized response, never Agent Media.

Cancel and Resume carry caller-preallocated request IDs. Resume also carries the
current rule revision and creates a new attempt. Neither client retries mutations
after an uncertain response. Cancel/Resume receipts contain exactly `id` and
`state`; malformed mutation replies retain the unknown-outcome contract. Blocked
check results cannot carry snapshots or assertion evidence. Policy reads/writes and detailed diagnostics are
Local-only; authenticated Devices need per-rule View, Cancel or Resume grants.

Control requests and responses obey the API foundation's 128 KiB bound. Policy
replacement also uses control admission; larger policies must be reduced before
submission. Data replies are bounded to 1 MiB. Policy persistence independently
limits the complete private document to 1 MiB.

Clients validate reply shapes, finite state values, canonical coordinates, page
ordering/watermarks, bounded text and canonical PNG base64 before exposing a
response. Transport byte bounds do not substitute for domain validation.
Nested deployment, standing-rule and checker evidence must have their complete
bounded wire shapes. Nullable fields accept only null or their declared type.
An invalid response after dispatching a mutation is `OutcomeUnknown`; a malformed
request or an invalid read response remains `Invalid`. Clients do not retry either
an uncertain transport result or an uncertain domain response.
