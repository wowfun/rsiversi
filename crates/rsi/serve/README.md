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
