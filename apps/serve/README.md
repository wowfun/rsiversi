# rsi-serve

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
the separately composed HttpAssets capability. The standard catalog exposes
this as `rsi.application.serve-web`, with `rsi.web.assets` owning the bundle.
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

The ordinary local Web factories own `--port` (default 8787, range 0..65535),
`--no-open`, `--assets ABSOLUTE_DIRECTORY`, and Web-only help. The launcher
supplies frozen executable-relative assets and SSH/browser-opening inputs.
Assets activate before the Service. Missing bundles identify the selected path
and build command. Opening never builds assets or replaces an existing Host.
The loopback listener derives its exact Origin from the socket it actually owns.

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
