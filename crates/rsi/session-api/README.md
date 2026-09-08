# rsi-session-api

Ordinary endpoint and client plugins expose the Session domain through registered
versioned operations. The server consumes the shared Session service and its
trusted ingress; it owns no draft table, execution pin, provider or durable log.
Creation passes the authenticated origin to the existing draft owner. The client
publishes only the application-facing Session capability.

Each attached handle carries an atomic Header and fingerprint binding. Handle
operations and stream items carry that fingerprint and exact Session identity;
operations reject a different Header under a reused Session identity. This
fingerprint identifies canonical Header content, not a recoverable draft pin;
the live draft lease remains the domain's idempotency boundary.
Transport EndpointId/HostEpoch checks independently fence the deployment generation.
Preset selection validates the returned Header as exactly the previous Header with
the selected preset, validates the successor draft revision, and replaces that
handle's binding atomically. Already admitted calls and streams retain their
captured binding. A stale selection response cannot overwrite a newer binding;
another handle refreshes through attach after a remote switch. Creation replies
echo the original creation input alongside the current draft snapshot, so a retry
after selection can return the actual draft without changing its creation identity.
All DTOs are closed; receipts, history, recent pages, inspections, observation
cursors and live interactions are validated against their request before exposure.
Message reads also echo the exact acceptance cursor. Invalid-input diagnostics
retain at most 4 KiB of UTF-8 text; clients reject an oversized diagnostic.

Command discovery, execution and receipt lookup use authenticated Data operations.
The adapter validates the request identity, command identity and complete invocation
digest on execution replies; uncertain or mismatched mutation replies preserve the
original request identity as `CommandOutcomeUnknown`. Lookup validates its exact
request identity and never repeats execution. Command revision conflicts echo the
caller's expected revision. Compact receipts do not contain domain state values.
Each command operation reserves the full Data scratch ceiling before attachment:
resolving a compact receipt or command catalog can still load Headers, canonical
controls and complete domain snapshots. Its wire response remains independently
bounded (512 KiB for discovery, 8 KiB for receipts).

The authenticated `session/projections` Subscription returns complete retained
extension snapshots with a separate 5 MiB payload plus 64 KiB envelope limit.
Both the envelope and snapshot bind the captured Session/Header; the client also
checks monotone draft or durable dual cursors. A preset change ends the old
subscription. Each decoder reserves its bounded projection collection before
JSON decode and retains the resulting snapshot before releasing wire ownership.
Server capture admission belongs to the Session service, independently of API
delivery-byte admission; idle streams reserve no maximum reply buffer.

Create, input, direct Image and interaction answers are owned mutations. Unknown
message outcomes retain the caller's MessageId for status/retry reconciliation.
No operation is replayed automatically. Short cancellation, message status and
interaction answers use the Control lane. Reads use Data, and observations use
Subscription. Each returned observation owns its decoded retention independently
of the temporary wire buffer; an idle stream does not reserve a maximum payload.
Finite handlers acquire their conservative materialization ceiling from separate
2 MiB Control and 64 MiB Data scratch pools before reading domain state. Scratch
waits occur only inside already admitted API calls and retain no domain result;
cancelling a read drops its wait. The permit lasts through serialization, then
only exact encoded delivery bytes remain charged. Maximum-size reads therefore
serialize their scratch work without rejecting each other or waiting for a
previous response's transport consumer. Mutations retain independent job ownership.

History pages fit within 64 MiB including their envelope. An oversized full page
drops its oldest prefix and reports more history, preserving contiguous backward
cursors. Recent reads cap the underlying count by the 1 MiB Header ceiling before
materialization, so a bounded reply cannot first collect 256 MiB of Headers.
Page count is an upper bound; callers continue using the returned cursor and
`has_more`. Atomic inspections and individual Facts retain their domain bounds.

Tests use the shared Session testkit against actual isolated services and domain
adapters. Transport-specific authentication, delivery and disconnect checks belong
to the API adapter fixtures.
