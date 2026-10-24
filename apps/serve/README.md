# rsi-serve

Serve owns connection negotiation and Web asset operations for its entire
generation. Closing or retiring a local registration reports that owner's state;
it never redirects those operations to a colliding parent service registration.

## Resident product listeners

The product launcher connects to the already selected local Host. Its ordinary
`rsi.service.web` plugin owns browser listeners and per-connection Serve/Desktop
listeners. Control frames are bounded, native-peer authenticated and fenced by
Host epoch plus the `rsi.web.control` descriptor. Applications do not create an
additional Store or rotate credentials when attaching.

`rsi web` starts or reuses one loopback listener, prints a fresh one-use ticket,
opens the browser in the caller's desktop environment and returns. Omitting
`--port` reuses the actual bound port; changing an explicit port or source requires
`rsi web stop` first. Stop retires only that browser listener. Serve and Desktop
instead retain a control connection; disconnect retires their own listener.
Serve is API-only by default. `rsi serve --assets ABSOLUTE_DIRECTORY` additionally
serves an internally verified Web bundle from that same authenticated listener.
Its explicit HTTP Origin and TLS policy remain unchanged.

Ordinary Desktop retains its loopback port in `state/desktop-port`, so the exact
Origin and WebView drafts survive both window and Host restarts. The first
listener selects a free port and durably publishes it before issuing a ticket.
This private record is exactly one nonzero big-endian u16; no-follow reads reject
malformed or nonregular entries. Publication syncs a private staging file before
linking it without replacement and syncing the state directory. A later launch
reclaims only the known regular `desktop-port.pending` staging entry. File work
runs on blocking workers under a Desktop creation coordinator; the selected Host
already holds exclusive state authority. A busy retained port fails explicitly,
without silently changing Origin. Thus one ordinary Desktop listener can be
active per state home. Connection loss still retires the listener, and `web stop`
does not affect it. Explicit source frontends use their configured Origin.

Asset presence never selects browser authentication: Serve retains device
authentication and its requested binding; only local Web/Desktop listeners
issue browser launch tickets.
There are at most 32 control connections, of which at most 16 can retain
connection-owned listeners. Long-lived attachments leave 16 slots for commands.
Excess connections or attachments receive a bounded capacity error. There are
at most 64 pending ten-minute tickets per listener. Repeated tickets share one Host-owned managed device credential.
Revocation prevents new tickets and successful redemption until an explicit Host
restart reauthorizes the managed device.

The default Web source is the installed `web` package's `web` export. An explicit
absolute `--assets` directory is local operator authority. Both sources verify
the internal bootstrap receipt; installed packages additionally retain their
immutable package closure and check required interfaces. Core executable identity
is not a Web bundle family.
The embedded local Web adapter applies the same internal bundle verification.
Its merged operation catalog uses the same Application-local precedence as
dispatch. A later Service generation publishing the same operation ID cannot
replace that local operation or erase unrelated discovery entries.

A watch invocation with an explicit source frontend recipe creates an independent
connection-owned listener. Its exact frontend Origin is validated before HTTP
activation; forwarded headers never select that authority. The caller's ordinary
Process/Sandbox children supervise Vite, including bounded control frames,
readiness deadlines, exit and descendant cleanup. The verified bundle supplies
Worker/WASM assets; frontend source changes use Vite's document reload.
This listener does not replace the persistent browser listener.

After a source package is admitted, `Refresh` re-resolves that package for all
live listeners using its export. Each listener stages a verified candidate and
publishes only renderer changes, retaining the new package lease. A changed
bootstrap reports restart-required and leaves the old generation serving.
The watching application then drains and re-resolves its own composition.

The following embedded SDK contracts apply only to explicitly composed
in-process services; product commands use the resident listener contract above.

Retained service observation and reload handles do not own the native service
lifetime. After service cleanup, observing its stopped result remains valid and
reload fails; neither operation pins the stopped generation's owner lock.

The native Serve application owns HTTP configuration, process signals and one
ApplicationRun entry. It consumes the API dispatcher, device authentication and
connection-description capabilities, plus a ServingService capability for the
chosen service generation's failure notification and Profile reload. It creates
no backend or Runtime: an ordinary
HttpFactory child owns the listener and its requests within the invoking Profile.
The standard product composes its independently owned service generation beside
this application.
Native signal ownership supports Unix and Windows. Windows handles Ctrl+C,
Ctrl+Break, console close, logoff and shutdown; the OS can bound the grace period
for the last three. Profile reload remains available through its API; SIGHUP is
Unix-only. Unsupported platforms reject before creating the HTTP child or listener.
Complete static HTTP/argument validation precedes platform admission. Malformed
configuration returns `MetaError::InvalidInput`; valid configuration requiring
unavailable signals returns `MetaError::Activation`. The retained application
diagnostic uses the existing `RsiError::Boot` class for Host bootstrap failures.
Activation failures also use `MetaError::Activation` and retain that diagnostic.

The factory validates arguments and HTTP policy during preparation, before any
backend activates. Configuration may be an HttpConfig object with no arguments,
or null with `--bind ADDRESS --origin ORIGIN` and either `--tls-certificate FILE
--tls-key FILE` or explicit `--dev-http`. Production requires TLS and the existing
HTTP/2 browser contract. Development HTTP requires loopback binding and origin.
Certificate contents are read by the HTTP owner at activation, never as arguments.

Calling ApplicationRun schedules its one owned entry before returning a waiter.
Dropping the waiter does not stop the listener. SIGINT or SIGTERM finishes the
entry, allowing the enclosing Profile to drain requests and shut down its owned
service. Listener failure returns a failed exit. Withdrawal fences entry, cancels
the signal waiter and drains owned work; it never signals another service process.
On Unix, SIGHUP starts at most one reload waiter. Stop remains selectable during
reload and drops that waiter before composition shutdown; a closed SIGHUP source
is disabled. The application republishes the read-only HttpListener observation
so another application plugin can discover the actual bound address.

ServeFactory::with_web_assets selects StaticHttpFactory and explicitly requires
the separately composed HttpAssets capability. Embedded SDK callers compose it directly
with `rsi.web.assets` owning the bundle. The product catalog uses the resident
`rsi.application.serve` entry instead.
Its argument and lifecycle contracts match Serve; it does not resolve asset
paths or read files itself. API-only Serve requires no Web build artifacts.

Serve Web additionally owns the authenticated renderer-generation API. Its scoped
HTTP dispatch combines the selected Service's live dispatch with an independent
Application registration owner for asset leases. Only `connection.describe` and
`connection.operations` are deliberately replaced to negotiate the combined
catalog; other duplicate operation identities reject activation. The endpoint
identity and Host epoch remain those of the selected Service. The listener and
lease registrations retire together; renderer publication preserves both. This
composition neither exposes the Service's registrar to the document nor changes
its domain ownership.

## Local Web launch

After frontend readiness, bounded stdout chunks are drained until EOF or child
exit. Unexpected output is reported once without echoing its contents; a log
line is not process termination, including bytes received in the same chunk as
the readiness frame. Before readiness, stdout carries only protocol frames.
Package closure verification runs on blocking
workers, leaving async runtime workers available for listener control.

The ordinary local Web factories own `--port` (default 8787, range 0..65535),
`--no-open`, `--assets ABSOLUTE_DIRECTORY`, and Web-only help. The launcher
supplies frozen executable-relative assets and SSH/browser-opening inputs.
Assets activate before the Service. Missing bundles identify the selected path
and build command. Opening never builds assets or replaces an existing Host.
The ordinary loopback listener derives its exact Origin from its own socket;
owned source frontends use the resident plugin described above.
Local listener failure reports the requested port and suggests `rsi web --port 0`
to select a free port, including Windows reserved-port failures.

Explicit startup rotates a product-managed browser device credential while
retaining its DeviceId, then grants configuration access through the product's
existing Local authority. Revocation during that run is never reversed. The
application retains one random 256-bit launch ticket for ten minutes, redeemable
once at that listener. It closes the ticket owner on retirement. Readiness
prints a fragment-bearing link; the secret is never placed in a query string.
Browser opening uses argument arrays, is bounded and best effort, and is skipped
under SSH. A used, expired or revoked link without a valid cookie requires restarting
`rsi web`; normal reloads and additional tabs recover through the cookie.
Redemption consumes the ticket before authenticating its device. A concurrent
revocation, deletion or credential rotation makes that token unusable; failure
does not undo revocation, issue a replacement token or restore the one-use ticket.

The automatic OS opener receives the launch URL as a process argument. Local
users able to inspect those arguments may see the unused ticket, including while
it remains in a browser's initial command line. Fragment transport prevents HTTP
URL logging, not local process inspection. `--no-open` avoids this automatic
handoff and leaves the explicit printed-link channel. The current opener does
not provide a private cross-platform credential-transfer channel.

Resident browser creation has a separate coordinator from published status.
Package hashing and listener activation never hold the status mutex. Stop fences
any in-flight creation before awaiting its cleanup; a cancelled creation cannot
publish readiness. Status returns Stopped for a terminated listener.

Embedded and resident launchers share one ticket implementation for entropy,
expiry, redemption and retirement. Embedded SDK composition remains a supported
public surface, exercised independently from the product resident launcher.
