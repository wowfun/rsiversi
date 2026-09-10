# rsi-api-http

Malformed Content-Type and any Content-Encoding are rejected before API lane
admission. After admission the registered operation still checks its exact JSON
or binary encoding. Listener connection failures include rejected connection and
handshake capacity, making overload visible even before an HTTP request exists.

StaticHttpFactory adds an explicitly required HttpAssets capability to the same
listener and delivery machinery. The ordinary HttpFactory remains API-only.
Assets use exact GET paths without queries, bodies, Range or encoded requests;
unknown paths return 404. Host/Origin checks apply before lookup. Assets are
public application code, require no device credential, and cannot expose API
operations. At most eight asset deliveries exist per listener, retaining their
slot through HTTP/2 buffering and flush. Asset bytes retain their provider's
immutable budget lease; no response copy or filesystem lookup occurs here.
Responses use a closed MIME set, no-store, nosniff and a same-origin CSP permitting
WASM compilation but no inline or remote scripts. The provider owns bundle input
validation, aggregate retention and retirement.
Assets use `Referrer-Policy: same-origin`: foreign requests disclose no referrer,
while browser Worker POSTs retain the concrete Origin required by authentication.
`no-referrer` causes Firefox to send `Origin: null` and cannot satisfy that gate.

Each listener exposes bounded monotonic diagnostics without request text or
credentials: HTTP 4xx rejections, HTTP 5xx failures, connection accept/codec/I/O failures,
and TLS handshake failures. Cancellation during listener retirement is not a
failure. A response counter describes the generated status, not successful peer
delivery; later write failure may also increment the connection counter. Unix
socket acceptance and process ownership diagnostics remain with their publisher.
TCP accept failures retry after 100 ms without cancelling existing connections;
shutdown and completed connection jobs remain observable during this backoff.

The native HTTP adapter routes authenticated POST requests to exact registered
operations at `/api/v1/<domain>/<name>/<version>`. Domain plugins own all business
DTOs. Native clients use a bearer token; browser login transfers that token into
an HttpOnly, SameSite=Strict cookie, with Secure enabled under TLS. Cookie calls
require the exact configured Origin and `X-Rsi-Csrf: 1`. Every request checks the
exact configured Host; foreign Origin, query parameters, conflicting credentials,
duplicate security headers and unsupported methods are rejected before dispatch.
Credentials never appear in URLs, diagnostics or serialized configuration.

`LocalHttpService` reuses this codec and dispatcher for Unix streams. It checks
same-UID peer credentials before HTTP admission, requires the exact opaque local
compatibility key and Host `rsi.local`, and supplies Local origin itself. It
rejects browser/device credential headers and cookie routes. TCP listeners never
accept local authority. The caller owns socket publication and generation
lifecycle; one request per stream retains its admission through completed writes.

`HttpFactory` is an ordinary Meta plugin requiring the dispatcher, device verifier
and the published connection description. Its listener
capability reports the bound address and eventual stop result without granting
shutdown authority. Cleanup withdraws that capability, cancels transports and
drains the listener task. Cancelling an application waiter cannot dispose the
independently owned HTTP listener.

Production listeners require explicit PEM certificate and key files. HTTP
and HTTPS origins must use their canonical URL spelling at configuration
admission: lowercase hostnames, normalized IP literals, no default port or trailing
slash. Noncanonical values fail before opening sockets or TLS files, matching
the native client configuration contract.
Insecure HTTP requires explicit development configuration, a loopback listener and a loopback
public origin. The adapter never trusts forwarded headers or silently assumes an
external TLS terminator. TLS negotiates HTTP/2 or HTTP/1.1; explicit development
HTTP uses HTTP/1.1. Multiplexed HTTP/2 streams own separate request admission and
delivery leases. HTTP/1.1 retains its one-request connection owner. Both have
bounded connection/header/TLS buffers;
one-minute body reception cannot be extended by trickling progress. Registered
admission precedes body retention and domain execution.
Body storage grows under incremental byte admission with fallible allocation,
including requests without Content-Length; the registered per-body maximum still
applies. Revocation ends reads and subscriptions; an admitted mutation retains
the registry's independent owner.
At most 32 unclassified owners cover TLS/header reception, unclassified
responses and idle HTTP/2 connections. An HTTP/2 connection transfers its idle
permit into its first stream; concurrent streams acquire their own permits.
After the last delivery completes its transport flush, the connection reacquires
an idle permit or closes. Every idle interval expires after ten seconds; active
subscriptions have no idle timeout. A stream that cannot acquire admission closes
its connection. Successful operation admission replaces this permit with the registry's
class/device lease, retained through connection completion. The total connection
ceiling is 128; slow data delivery cannot consume the control lane's slots.
A write has one absolute 30-second deadline through its next completed flush;
partial progress cannot renew it, while an idle subscription has no write timer.
HTTP/2 also observes delivery deadlines independently of socket polling: a finite
response has 30 seconds from publication through its final flush. A subscription's
opening frame and each subsequent batch of ready frames have the same absolute
deadline until all queued frames leave the codec and flush succeeds; waiting for
the next domain event has no deadline. Flow-control stalls expire even if the
codec never polls the body. Expiry closes the connection and drains its tasks,
so other streams sharing that connection also disconnect.

HTTP/2 limits each connection to 32 concurrent streams, 64 KiB connection and
stream receive windows, 16 KiB frames, 32 KiB header lists and a 4 KiB HPACK table.
Its stream send buffer is at most 64 KiB. Runtime-owned HTTP/2 tasks are fenced and
drained with their connection. Body frames retain request authority while queued
in the codec; after their final owner drops, authority remains retained until the
transport's next completed flush or socket disposal. A connection-wide lease
cannot represent independently multiplexed operations.

`connection/describe/1` and `connection/operations/1` are ordinary registered read
operations owned by the independent API connection plugin. The HTTP listener
consumes them and never registers or retires them. The former negotiates wire
version and endpoint/generation identity;
all other domain calls require the negotiated `X-Rsi-Host-Epoch`. Operation versions
describe wire compatibility, not executable equality. Local automatic owner reuse
retains its independent executable/launch-key checks.

Finite JSON replies retain exact Rust JSON bytes. Binary replies use
`application/vnd.rsi.binary`: an eight-byte big-endian metadata length, an
eight-byte big-endian binary length, then those exact payloads. Subscriptions use
POST fetch SSE. The server immediately writes `: ready\n\n` before polling its
domain source so browser Fetch can resolve even for an idle subscription. This
fixed opening comment carries no event or sequence number. It is followed by
`item`, `error` or `domain-error`, then an explicit `end`
event. EOF alone is never success. Binary payloads use finite operations. Transport
adapters transfer retained byte owners without concatenating whole responses;
transport headers and TLS buffers have separate fixed bounds.

Run the native HTTP tests with isolated loopback listeners and fixture credentials.
They cover front-door rejection, real disconnect, admission, revocation, binary/SSE
framing and TLS. Application, browser-client and live-provider evidence remain
with their owning integration fixtures.
