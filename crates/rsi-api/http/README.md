# rsi-api-http

A resident application route can replace its process after disconnection.
Each incoming request selects one process generation. Replacement retires
that generation's requests, sockets and resources and forwards subsequent
requests to the new generation.

Application assets include WOFF, WOFF2 and TrueType fonts with their fixed MIME
types. Their URL authority and retained-byte limits are unchanged.

`LoopbackBrowserOrigin` admits an explicit canonical loopback HTTP frontend
Origin for a caller-owned local proxy. The supplying application must establish
and retain that proxy's socket ownership before constructing the listener.
`LocalBrowserHttpFactory` otherwise derives its Origin from its own socket.
Both modes keep the same exact Host/Origin, device cookie and CSRF checks;
forwarded headers cannot select or override the admitted Origin.

Malformed Content-Type and any Content-Encoding are rejected before API lane
admission. After admission the registered operation still checks its exact JSON
or binary encoding. Listener connection failures include rejected connection and
handshake capacity, making overload visible even before an HTTP request exists.

Finite operation replies declare their exact Content-Length, including the
16-byte prefix of a binary reply. This length survives delivery wrappers and
HTTP/2 framing so clients can admit the actual payload before polling its body.
Delivery guards still retain admission through buffering and completed flush;
the declared length does not shorten that ownership.

StaticHttpFactory adds an explicitly required HttpAssets capability to the same
listener and delivery machinery. The ordinary HttpFactory remains API-only.
An application may additionally supply a private loopback upstream for a closed
set of route prefixes. A trusted application can exercise host-user privileges.
Every authenticated browser uses the deployment's shared browser owner for
native operations, subscriptions and application callbacks. Authentication,
expected-device checks, `connection/caller` and revocation still use the actual cookie credential.
An application request that supplies `X-Rsi-Expected-Device` must match that
credential before the listener forwards it. Browsers may omit this header.
Bearer API calls retain their registered device identity. The product supplies
its managed browser owner. An independently embedded HTTP server scopes its
default browser owner to its stable deployment endpoint. Browser requests must
pass the public Host/Origin policy and the resource's own authorization rules.
A browser credential is never copied to the application process.
WebSocket upgrades require the listener's exact Origin and device cookie.
If the client sends Fetch Metadata, its site must also be same-origin.
HTTP request/response bodies and WebSocket upgrades stream independently from
process-extension JSON frames. Exact static files and native API/bootstrap
routes retain their own handlers.
For authenticated GET and HEAD requests, a successful JavaScript or CSS
response that its publisher explicitly marks public and immutable may be
retained in the browser's private cache. For unversioned JS and CSS, an ETag
and no-cache allow private storage with authenticated revalidation on every
reuse. Its 304 reply retains that policy. Other upstream responses use no-store.
Assets use exact GET paths without queries, bodies, Range or encoded requests;
unknown paths return 404. Host/Origin checks apply before lookup. Assets are
public application code, require no device credential, and cannot expose API
operations. At most eight asset deliveries exist per listener, retaining their
slot through HTTP/2 buffering and flush. Asset bytes retain their provider's
immutable budget lease; no response copy or filesystem lookup occurs here.
Responses use a closed MIME set, no-store, nosniff and a same-origin CSP permitting
WASM compilation but no inline or remote scripts. The provider owns bundle input
validation, aggregate retention and retirement.
`TrustedExtensionApplication` is an explicit document-wide policy for installed
trusted UI code requiring inline style elements and attributes. It retains the
Application script, frame and resource restrictions. Selecting it grants no
isolation between extensions and the application. The default Application policy
continues to forbid inline styles; a bootstrap policy change remains an
application-restart boundary owned by the asset publisher.
`AssetType` owns MIME selection shared by HTTP and native document transports.
SVG application assets use `image/svg+xml`; their bytes follow the same immutable
delivery, budget and document policy as the other admitted application assets.
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
operations through the [shared OperationCall path codec](../protocol/README.md).
The API v2 path carries protocol name/version, operation domain/name/version and
call shape. Domain plugins own all business
DTOs. Native clients use a bearer token; browser login transfers that token into
an HttpOnly, SameSite=Strict cookie, with Secure enabled under TLS. Its name
includes the hash of the validated authority, isolating listeners on separate
ports of the same host. Its path is `/` so authenticated application routes
receive the same credential. Native API cookie calls
require the exact configured Origin and `X-Rsi-Csrf: 1`. Every request checks the
exact configured Host; foreign Origin, query parameters, conflicting credentials,
duplicate security headers and unsupported methods are rejected before dispatch.
Durable credentials never appear in URLs, diagnostics or serialized configuration.

`X-Rsi-Expected-Device`, when present, must be one canonical DeviceId matching
the authenticated caller before dispatch. It narrows a connection; it never
authenticates a request or supplies Local origin. Cookie logout requires this pin
and checks it before clearing a present device cookie; an absent cookie is an
idempotent success even without a pin.
This prevents an old connection from using or clearing another device's cookie.
It does not order already-sent cookie responses across browser tabs.

`LocalHttpService` reuses this codec and dispatcher for Unix streams and authenticated
Windows pipes. It checks same-UID Unix peer credentials before HTTP admission;
Windows streams must come from the [local transport](../local-transport/README.md)
and its process-identity checks. It requires the exact opaque local
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

Loopback browser subscriptions use an independent WebSocket connection per
operation, so idle subscriptions do not exhaust the HTTP/1 browser Fetch pool.
Only registered read subscriptions may upgrade; finite calls and mutations keep
the existing HTTP path. Exact Host and Origin, the HttpOnly device cookie, and
the expected device and Host epoch are checked before admission. Authentication
precedes the epoch comparison, so unauthenticated requests cannot probe it. The closed
`rsi-stream-v1` subprotocol carries those non-secret fences and the exact input
length; no URL contains credentials and no remote request obtains Local authority.
Socket authentication consumes the cookie and parsed device directly, requiring
the exact browser Origin. It does not synthesize HTTP CSRF or expected-device
headers; those remain the ordinary HTTP authentication contract.
Input admission and separate transport scratch precede message allocation.

The first binary message is the bounded request. The server returns a bounded
typed response head, then sends at most one 64 KiB body chunk per one-byte pull.
An empty chunk is EOF; the existing finite/error and SSE codecs remain authoritative.
Extra credit, text messages, wrong lengths and malformed handshakes close the
connection. Ping and Pong control frames are handled independently of application
input and credit, including while domain work is pending. They neither grant
credit nor restart the handshake deadline. Response heads are measured against
their fixed admitted bound before encoding. Each socket retains its ordinary connection and operation admission;
disconnect, revocation, retirement and listener shutdown cancel the read and
release them. There is no reconnect or replay. The handshake/input deadline is
15 seconds, writes retain the existing bounded-write deadline, and idle admitted
subscriptions have no read deadline. TLS browser traffic continues to require
HTTP/2 and does not use this loopback-only transport.

`connection/describe/1`, `connection/operations/1` and `connection/operation/1` are ordinary registered read
operations owned by the independent API connection plugin. The HTTP listener
consumes them and never registers or retires them. The former negotiates wire
version and endpoint/generation identity; the single-key policy query stays available
on Control when Data directory reads cannot be admitted;
all other domain calls require the negotiated `X-Rsi-Host-Epoch`. Operation versions
describe wire compatibility, not executable equality. Local automatic owner reuse
retains its independent home authority, process identity, Host epoch and
launch-key checks; executable build identity does not substitute for interfaces.

Finite responses and SSE errors share the HTTP-local encoding of redacted API
error codes. HTTP status and SSE framing remain transport-specific; Domain errors
transfer their retained payload unchanged through their distinct response forms.

Finite JSON replies retain exact Rust JSON bytes. Binary replies use
`application/vnd.rsi.binary`: an eight-byte big-endian metadata length, an
eight-byte big-endian binary length, then those exact payloads. Subscriptions use
POST Fetch SSE, or carry that same SSE body through the loopback browser socket.
The server immediately writes `: ready\n\n` before polling its
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

Static assets are available only through `HttpServer::with_assets`; the Unix
`LocalHttpService` has no asset provider or browser origin and serves API exchanges
only. Its GET behavior is independent of the document policy.

Immutable document assets carry a closed security-policy classification. The
application document retains its strict script policy and admits product-selected
root asset paths or explicit single-segment directory prefixes as frames.
A trailing slash selects a prefix; root, traversal, URL and CSP syntax are rejected. Local and HTTPS-enabled sandbox
documents have independent response CSP and opaque script-enabled sandboxing.
Raw data image URLs are excluded; the preview owner supplies admitted embedded
images as bounded Blob resources. HTTPS opt-in grants remote scripts, styles,
images, fonts and connections. Supplied or remote scripts can send previewed
contents to HTTPS servers; this is broader than remote image access.
The classification grants no API authority and never accepts request-authored
CSP strings. Product asset providers own which exact paths receive each policy.

An explicitly injected BrowserBootstrap capability enables the loopback-only
`POST /api/v2/browser-bootstrap` route. It requires an empty body, exact Host and
Origin, and `X-Rsi-Csrf: 1`. A single bounded `X-Rsi-Launch-Ticket` header exchanges
a one-use application ticket for a device cookie. Tickets are exactly 64 lowercase
ASCII hexadecimal characters; malformed tickets are unauthorized before invoking
the redemption capability. Without that header
a valid cookie returns only EndpointId and DeviceId for recovery. Bearer
credentials and expected-device mismatches are rejected. A ticket can replace an
old browser cookie after explicit credential rotation; an expected-device header
is forbidden for that exchange. An explicit ticket takes precedence even when the
browser sends a valid cookie. A rejected ticket never falls back to that cookie;
ambiguous duplicate cookies are rejected before ticket consumption. Ticket issuance and
configuration grants are outside the transport. Other listeners do not expose
this route. Durable device credentials remain forbidden in URLs; the local Web
application may hand off its short-lived bootstrap ticket in a URL fragment,
which the document removes before starting the Worker.
Application processes may own unversioned `/api` resources. Versioned native
`/api/vN` paths are always dispatched by RSI.
A preparing application may declare prefixes before publishing its loopback
endpoint. Waiting requests consume the same per-process capacity and stop
on device revocation or retirement. The published endpoint must own each
forwarded path.
An authenticated user may open an application document directly from the
location bar. The read-only navigation carries Fetch Metadata with site
`none`, mode `navigate` and destination `document`.
The native listener supplies an opaque request handle alongside the admitted
device. It resolves to that caller only while the response body or upgraded
socket is alive. Revocation and process retirement invalidate the handle.
The connection deadline bounds opening the private socket. Once forwarded,
an installation can take as long as its application operation permits. Device
revocation and application retirement still terminate the wait.
The HTTP-only, SameSite=Strict device cookie uses path `/` and a name
derived from the validated public authority. Independent local listeners
on the same hostname use separate cookies. The native forwarder strips
all cookies before sending a request to an application process.
An application request may retain bounded process-local resources by key. An
explicit release or retirement of its exact upstream request drops those
resources. Resource release does not grant authority to invoke an operation.
A retaining caller supplies an `Arc<T>`. A continuing operation borrows the
same type and receives a shared handle tied to that request's retirement.
Its lifetime token is cancelled when the key or its physical request retires.

Native application composition may also retain an `ApplicationCallback` for an
explicitly supplied device and lifetime. This uses the same bounded callback
and resource pool as forwarded requests, but its guard owns the lifetime instead
of an HTTP response. Cancelling the supplied lifetime closes access and cancels
resource work. Dropping the guard or retiring the application also releases
its retained resources. The transport
does not choose that principal or upgrade it to local authority. Browser cookies
and expected-device checks still apply to every incoming browser request.
