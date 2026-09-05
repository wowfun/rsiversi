# rsi

The `rsi` product is the standard RSIversi local Session application and Host.
Its library owns the explicit linked factory catalog, standard composition,
closed Application and Host Profile catalogs, the transport-independent
Session interface, and both process-local and Unix-domain-socket adapters. Its
binary owns CLI parsing, line-oriented terminal interaction, process signals,
and construction of the Tokio runtime.

The standard catalog links OpenAI, OpenAI-compatible, and DeepSeek factories
without implicitly enabling a deployment. A persistent Profile instantiates
the chosen provider and Settings names an exact default deployment/model.
The standard Host explicitly configures four maximum active Agent turns. The
Kernel still serializes turns within one Session; the four lanes permit bounded
progress across independent Sessions. A copied Host Profile may replace the
complete executor configuration to select another value from `1..=256`.

`rsi --profile NAME [application arguments]` selects one Application Profile.
The built-in, non-shadowable `session` and `headless` profiles both select the
built-in `standard` Host Profile. There is no implicit Application Profile.
Application Profiles are bounded TOML documents below
`application-profiles/<id>/application.toml`; Host Profiles are bounded TOML
documents below `host-profiles/<id>/host.profile.toml`. Profile management can
list, inspect, copy, delete, and purely preview these documents without
activating plugins, resolving credentials, acquiring the Store lease, or
publishing a Host endpoint. Host preview reads the authoritative Agent-preset
Settings used by daemon launch, but it does not materialize the built-in preset
asset or activate the selected Host Profile.
Catalog listing includes only regular profile documents; a symbolic-link
document is neither opened nor advertised as an available profile.

The exact management surfaces are `rsi profile application
<list|show|path|copy|delete>` and `rsi profile host
<list|show|path|copy|delete|preview>`. `rsi host start` is the only operation
that detaches a new daemon; `serve` runs it in the foreground, `status` probes
the recorded generation, `reload` requests a full Profile rebuild, `stop`
drains it, and `restart` composes stop and start. `stop --force` and `restart
--force` open a pidfd and validate the recorded process start token before
sending `SIGKILL` to that exact process descriptor. If the runtime's SIGHUP
source closes, the daemon disables only the reload branch after one diagnostic;
it does not spin on an always-ready closed stream. SIGTERM/SIGINT closes reload
admission and aborts any in-flight SIGHUP waiter before daemon shutdown, so a
stalled reload cannot retain the Profile lifecycle lock ahead of stop.
The `host start` launcher reserves the owner lease before spawning and passes
it to the child without releasing ownership. The child creates a new Unix
session before Host bootstrap, so
terminal process-group signals and hangup ownership do not remain shared with
the launcher. Foreground `host serve` deliberately keeps its caller's session.

One owner process holds the standard Host paths at a time. A foreground daemon
publishes a same-user Unix-domain socket; an application uses it when its exact
protocol, product build, Host launch key, and Host epoch handshake is
compatible. Durable metadata remains structurally readable across executable
rebuilds so lifecycle commands can identify and signal an older exact process
generation; compatibility is enforced during application selection and the
handshake. The active daemon's validated metadata endpoint is authoritative,
including when the client's runtime-directory environment differs or cannot
itself hold a Unix socket. With
no owner, an application may acquire the same owner lease and
run a private embedded Host without publishing an endpoint. A starting,
embedded, or temporarily unresponsive owner is waited for up to the same
15-second readiness bound and is never bypassed by a second Host. The standard
product daemon is Linux-only because its lifecycle
signals are fenced by a pidfd plus Linux process start identity; other
platforms support embedded mode only.

The Session interface creates, attaches, and lists sessions, then exposes one
handle for ordered text-and-image mailbox submission, direct Image generation,
cancellation, reconnectable observation, bounded backward history, and live
approvals. Callers allocate a `MessageId` for Language or multimodal input.
Acceptance atomically persists the immutable Header when needed and a canonical
mailbox record, but it does not invent a `TurnId`; the later durable claim
creates the Turn and first Step. Retrying the same identity and body returns the
indexed message state across reconnect or restart, while a changed body is a
typed conflict. Agent-control records and Facts are independent durable streams.
Approval waiters remain bounded live Host state and are never replayed as
effects.

The interactive `session` application is line-oriented. Ordinary lines are
accepted FIFO through a 16-message application queue and a one-line reader
handoff; the blocking stdin producer stops reading while both are full.
`:queue`, `:cancel`, `:approvals`, `:allow`, `:deny`, `:exit`,
and `:help` are local commands, while `::` escapes a leading colon. Ctrl-C
cancels only this client's tracked message or its claimed Turn, and detach never
cancels work. The `headless` application accepts one message, may attach
repeatable `--image` inputs before admission, and has no approval capability; an
unanswered approval remains pending until cancellation or Host shutdown rather
than being denied merely because the submitter is headless.
On Unix, each image path is opened no-follow and nonblocking before its handle is
verified as a regular file, so a FIFO, device, or final symlink cannot occupy a
blocking worker while waiting to be classified.

Application exit status 0 means a completed turn; an interactive Session also
treats its user's locally cancelled turn as a successful control action. Status
1 covers failures after the application has acquired its Session surface,
including submission-time route or provider configuration rejection and a
failed, partial, interrupted, or budget-exceeded terminal outcome. Status 2
covers command-line, Profile/catalog, and Host bootstrap failures before that
handoff. Status 130 means headless signal cancellation.

The Rust Session interface additionally exposes direct Image generation. Its
caller allocates the `TurnId`; the operation validates its exact Image route and
does not require the session's default Language deployment to be available.
Image results remain Media references.

The product materializes its built-in `standard` Agent preset as a verified,
digest-addressed cache asset and prepends it before configured and writable
user roots. Unix materialization creates, verifies, and publishes through
no-follow directory descriptors. It accepts an operating-system alias only in
the first component below `/`, then rejects symbolic links throughout the
owned suffix. The portable fallback rejects observed link
or reparse-point components before publishing. Each fresh session retains a
process-local draft carrying the current preset generation until its first
submission is durably accepted; a failed pre-durability attempt can therefore
retry through the same handle without resolving a different generation.
Durable resume uses the Header's required
`agent_preset_id` and cannot override it. The Kernel retains that exact pin for
the resident session, while the executor reads definitions and executes every
Tool through the claim's immutable catalog. The runner prepares that exact
fresh or resume generation before any durable Workspace registration, and a
generation-preparation failure therefore cannot create a Workspace row.
Dropping an unsubmitted resume token has no Store or resident-capacity side
effect.

On Linux, linking the standard coding Tools makes a successfully probed
restricted sandbox backend a Host activation requirement. The Host does not
begin serving and defer an unavailable enforcement backend until the first
Tool call. On Linux, the binary resolves its own canonical executable and `/bin/bash`,
freezes the scrubbed child environment before Host construction, and passes
those values explicitly into the standard composition. The Bash Job producer
is global because Jobs identities outlive Agent generations. The model-facing
`bash`, three Jobs controls, and `apply_patch` are separate Agent-only
contributions activated inside an unpublished Tool catalog and atomically
sealed with one preset generation. Other platforms omit the Linux-only Bash
and apply-patch contributions before Runtime mutation rather than advertising
effects whose native lifecycle guarantees were not tested.
The Session Jobs finalizer cancels and reports all unfinished turn work before
the terminal Fact; unreported background completion blocks a successful turn.
These closure claims require the host process to remain alive through
finalization. Restricted standard plans also bind Bubblewrap to parent death;
`danger-full-access` has only process-group ownership and cannot honestly claim
cleanup after host `SIGKILL` or containment of a descendant that calls
`setsid(2)`. Web, TCP, cloud identity, marketplaces, arbitrary executable
profile bundles, Media export, and native package management are outside this
Host contract.
