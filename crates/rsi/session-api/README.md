# rsi-session-api

`session/export` v1 is an authenticated Subscription Read bound to the attached
Session/Header. Requests fit 8 KiB; each JSON stream item fits 512 KiB, including
worst-case escaping of a 64 KiB chunk. Clients check start identity, options,
offsets, terminal byte count and digest and reject EOF without completion.
The [export owner](../session-export/README.md) defines content and source cuts.

The closed `setup_required` domain failure preserves `SessionError::SetupRequired`
across local and remote creation. Backend diagnostics remain redacted and retain
their API failure category. Clients may show setup without treating data or Store
failures as missing configuration.


The current Session format is 16. Operations negotiate their own versions:
create v7; attach, recent, draft-snapshot, select-preset and inspect v6; history,
observe and read-message v5; submit v4. Message-status is v3. Metrics is a read-only v1 operation returning a
validated fixed watermark, progress and checked Session totals in at most 64 KiB.
Evidence is a read-only v1 operation: one exact request section, at most 256 KiB
decoded source per page and a 2 MiB encoded response ceiling. Clients revalidate
source identity, byte offsets, UTF-8 progress and package-level unavailability.
Other schemas retain their existing versions. ModelEvent purpose tags remain
present in bounded partial history and observation payloads.

Reference-capture, reference-preview and reference-read are authenticated finite
Data/Read v1 operations. Capture binds a durable source to the target's actual
Header. Preview accepts that immutable draft descriptor; recorded reads accept
only exact Session/Fact/content-index coordinates in the current or inherited
direct-parent interval. Clients validate target and source binding, request echo,
preview bytes, page offsets and progress. Requests and responses fit their
explicit 8/64 KiB input and 64/512 KiB output ceilings. Capture may leave an
unsubmitted immutable CAS object after cancellation under retain-all ownership.
The authenticated Session service is the authority for CAS contents. Page checks
detect malformed or mismatched responses; they do not authenticate arbitrary
tail bytes against the envelope digest independently of that service. Pages
carry neither the full envelope nor a cryptographic range proof.

Ordinary endpoint and client plugins expose the Session domain through registered
versioned operations. The server consumes the shared Session service and its
trusted ingress; it owns no draft table, execution pin, provider or durable log.
Creation passes the authenticated origin to the existing draft owner. The client
publishes only the application-facing Session capability.

`SessionTargetClient` narrows an existing client or an explicitly supplied local
dispatch/origin pair to one Session. It advertises only attach and handle
operations, never creation or deployment-wide recent listings. It validates the
closed Session target envelope before dispatch and rejects another Session ID;
the ordinary handlers still validate the current Header fingerprint and every
domain input. Local construction reuses the registry's input pools and invocation
ownership. It never derives caller origin from JSON or a presentation identity.
The grant preserves that handle's domain authority: approval answers may name
the attached Session or a descendant in its current Agent tree. The Session
service validates that membership and the exact owner/approval tuple; unrelated
owners are rejected. Cancellation and commands remain scoped by that handle.
Reference capture through this narrowed grant may use only the bound Session as
its source. Cross-Session capture requires the full application connection;
knowing a source Session ID does not extend a contribution's read authority.
Preview and recorded reads retain their exact target and inherited-input checks.
`SessionClient::attach_target` consumes this smaller catalog and returns only the
attached handle. Exporters retain normal domain receipts and never replay a call.

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

`goal-control` is an owned Data mutation carrying the immutable Goal request and
expected command revision. It reserves the command materialization ceiling and
returns a verified canonical command receipt plus a separate small live snapshot.
`goal-status` is a Control read and `goal-observe` is a disposable Subscription;
neither arms a controller. All three are version 1, bind the exact Session/Header,
and limit live diagnostics to 4 KiB. A malformed control reply preserves the
original request as `CommandOutcomeUnknown`. Disconnect never retries or cancels
an owned Goal. Durable Goal values use the existing projection subscription.

`jobs` v1 is a finite Data read for the exact current active Turn. Its capture
and page bounds follow the [shared Jobs contract](../session-protocol/README.md),
with a separate bounded transport envelope.
Both sides validate Session/Header/Turn, claim generation, cursor, ordering and
summary semantics. The decoder reserves its independent Jobs page budget before
decoding; retained snapshots own the final bytes after transport delivery ends.
Unavailable claims are not read from history, and no output or reporting operation
is exposed by this status port.

The authenticated `session/projections` Subscription returns complete retained
extension snapshots with a separate 5 MiB payload plus 64 KiB envelope limit.
Both the envelope and snapshot bind the captured Session/Header; the client also
checks monotone draft or durable dual cursors. A preset change ends the old
subscription. Each decoder reserves its bounded projection collection before
JSON decode and retains the resulting snapshot before releasing wire ownership.
Server capture admission belongs to the Session service, independently of API
delivery-byte admission; idle streams reserve no maximum reply buffer.
Fact/control and interaction items follow the same separation: their already
admitted immutable payload is measured, then encoded under its exact wire charge
and the registered maximum. Domain capacity remains a Session failure; API
delivery capacity remains an API failure throughout an opened stream.

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

Goal status and observation replies each allow 16 KiB encoded, including the
target envelope and maximum JSON escaping of the 4 KiB diagnostic.

Live-preview admission uses a monotonic native/browser clock; deterministic tests
pass the observation time explicitly without sleeping or requiring a Tokio timer.

The finite `resource` operation reads the selected Session's pinned resource
catalog or one resource preview. Requests and replies use the validated resource
protocol, require the Session read grant, and retain generation-bound identities.
The narrowed Session-target adapter exposes this operation only for its bound
Session; discovery and previews neither submit a Turn nor authorize execution.

Terminal output long polls use authenticated Subscription admission, leaving Data
slots available for finite Session reads and submissions. The terminal-output v2
wire operation emits one page with a 128 KiB encoded ceiling, then closes. The
Session client consumes its first page and drops the stream; its public method
remains a finite read, independent of transcript subscription ACKs. Terminal
controls use Control mutations with an 8 KiB encoded request ceiling. Input uses
a distinct Data mutation with a 512 KiB encoded request ceiling covering the
64 KiB byte array. All retain the exact Session/Header target.

Clients validate operation-specific terminal, attachment, stream, controller and
input receipt coordinates before exposing replies. Unsupported terminal policy
and native failures retain the bounded typed terminal error taxonomy.
