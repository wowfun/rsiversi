# Standard composition and Profiles

The standard Linux coding catalog includes `output_read`, an independent
read-only contribution over the Process completed-output cache. It accepts only
an issued output identity, raw offset, and bounded page size. Complete logs live
under the Host cache identity and may survive normal exit/restart until quota
eviction; they are not a durable Session archive. Model text includes safe UTF-8
decoding and raw cursors. A page that splits a UTF-8 character may display the
replacement character U+FFFD; the next cursor always advances by raw bytes.
Tool results and `:output` include `bytes_hex` for exact reconstruction. When
decoding or display sanitization changes a page, the Tool's model-facing text
also carries those hex bytes. Display text replaces control characters and
Unicode bidi controls, while preserving newline, tab, and ordinary joiners.

The `rsi` product owns standard application composition and the Service Host.
Its library owns Service composition and metadata-driven Profile management.
The [application catalog](../../../../apps/catalog/README.md) owns official
Application factories and built-in Application Profile documents. The library owns the transport-independent
Session domain, and process-local and Unix-domain-socket adapters for independent
Workspace, Models, Media and completed Process output capabilities. Application entry ownership follows the
[CLI](../../../../apps/cli/README.md) and [terminal](../../../../apps/terminal/README.md) contracts.
The Host [Goal](../../goal/README.md) and [Schedule](../../schedule/README.md) controllers
retain separate live continuation owners. The standard composition supplies the
opt-in Agent [Program runtime](../../../rsi-agent/program/README.md) with native Process
and Jobs services; workflow state and recovery remain in Agent Kernel.
The standard Host catalog maps Session, ingress and finite-read Local contracts
into the same Host Profile isolation scope. Multiple Hosts inside one Runtime
therefore share no raw Session read mapping.
`RunningRsi::inspect` combines bounded redacted Meta ownership metadata with the
existing Profile status and desired tree. Embedded Hosts inspect only their real
scope; separately owned Hosts can include their global Runtime resource counters.
The observations are captured at their owning boundaries, not as one atomic graph.

The standard Host supplies the independent Files reader and Session-bound Files
API. Embedded, UDS and HTTP applications use the same finite typed client.
Authenticated users may browse valid Session roots;
expired drafts remain unavailable. The standard Unix Agent preset includes
`file_read` and `directory_list` through its sealed Tool catalog and existing
approval/Sandbox policy. Other platforms expose typed unsupported reads and omit
these Tools from the standard preset. Reader guarantees and limits belong to
[Files](../../../rsi-files/README.md), and authentication/binding belongs to
[Session Files](../../session-files/README.md).

The standard catalog links OpenAI, OpenAI-compatible, and DeepSeek factories
without implicitly enabling a deployment. A persistent Profile instantiates
the chosen provider and Settings names an exact default deployment/model.
The standard Host explicitly configures four maximum active Agent turns. The
Kernel still serializes turns within one Session; the four lanes permit bounded
progress across independent Sessions. A copied Host Profile may replace the
complete executor configuration to select another value from `1..=256`.

Application-specific linked plugins are supplied through `ApplicationComposition`,
which carries an unchanged `StandardComposition` and a validated Application-only
addon set. They participate in Application preflight, native-name reservations
and catalog refresh. They do not enter the Service catalog digest or Host launch
identity. Service and Agent declarations and domain exports cannot be supplied
as Application extras. Application composition explicitly requires its catalog
provider; Service composition does not select an application.

`rsi --profile NAME [application arguments]` selects one Application Profile.
The [official catalog](../../../../apps/catalog/README.md) owns non-shadowable
built-in selections and their Service connections. There is no implicit
Application Profile.
Application Profiles are ordinary ordered Profile programs below
`application-profiles/<id>/application.profile.toml`. They declare the connection
and application plugins, with the same groups, isolation, Rhai expressions and
relative sources as any other Profile. Every initial leaf prepares before any
backend activates. The old application enum and `application.toml` documents are
unsupported; recreate them explicitly. The retired `session` application name
is rejected with guidance to select `cli`; old files are never overwritten.

Host Profiles are bounded TOML
documents below `host-profiles/<id>/host.profile.toml`. Profile management can
list, inspect, copy, delete, and purely preview these documents without
activating plugins, resolving credentials, acquiring the Store lease, or
publishing a Host endpoint. Host preview reads the authoritative Agent-preset
Settings used by daemon launch, but it does not materialize the built-in preset
asset or activate the selected Host Profile.
Catalog listing includes only regular profile documents; a symbolic-link
document is neither opened nor advertised as an available profile.

On Unix, `ProfileCatalog::preview_host_edit` and `preview_application_edit`
select one existing user document and borrow an explicit frozen Host. Builtins
are immutable; includes and linked fragments have no writable selection. The
consuming edit value exposes bounded original/proposed source bytes and the
Host's redacted effective tree diff and factory identities. Its Debug output
omits source and configuration. Preview does not prepare configuration semantics,
activate plugins, create temporary files, or acquire a persistent write lock.
An invalid old source can be repaired if the proposed program compiles/resolves.

`preview_host_leaf_edit` narrows this authority to an existing plugin leaf in a
writable user Host root. It preserves the source's comments and prior steps,
appends a strict root override, and prepares the enabled proposed graph before
returning the ordinary consuming source transaction. Configuration replacements
are literal exact JSON, limited to 64 KiB, 32 levels and 4,096 values. Instance
identity uses the configuration API validator. Successful edits reuse the prior
tree from the reviewed preview rather than compiling a separate unchanged edit. They are
prepared even for a disabled target. Enabling under a disabled ancestor returns
that ancestor as a blocking reason; the operation never edits a group, include,
Application Profile or resident Session. Preparation proves configuration
acceptance, not successful runtime activation.

`commit_once` acquires a nonblocking cooperative lock on the opened parent
directory, verifies that directory identity, the original root digest, and all
captured prospective source fingerprints against the same frozen Host, then
stages a private sibling, syncs it and atomically replaces only the selected root
through its directory handle. Symlink components and special files are rejected.
Conflicts require a fresh preview; the edit value cannot be replayed or retargeted.
The review digest binds the native parent directory identity as well as source,
dependencies and frozen composition, including when a retained proposal is reconstructed.
The source limit is the catalog's existing document bound. Includes remain subject
to the supplied Host's compiler bounds. Locking coordinates cooperating writers;
it is not an atomic compare-and-swap against arbitrary external file writers.

A successful receipt means source publication and reports directory durability
separately. It does not report Runtime activation: apply, restart, rollback and
degraded outcomes remain with Profile control. Neither commit nor activation
failure rewrites the previous source. Parent renames cannot redirect the handle's
write authority. This writer uses Unix directory-handle operations; no equivalent
Windows writer is exposed.

The exact management surfaces are `rsi profile application
<list|show|path|copy|delete>` and `rsi profile host
<list|show|path|copy|delete|preview>`. On Unix both kinds also provide
`preview-edit ID SOURCE_FILE` and `commit-edit ID SOURCE_FILE REVIEW_DIGEST`.
The preview prints original/proposed source, redacted effective changes and a
digest binding the original root, complete proposal and frozen composition.
Commit recomputes that preview and requires the exact reviewed digest before
consuming it. The digest is a comparison token, not an authorization credential.
The command reports `runtime: not_requested`; an existing source watcher may
independently observe the publication. `rsi host start` is the only operation
that detaches a new daemon; `serve` runs it in the foreground, `status` probes
the recorded generation, `reload` requests a full Profile rebuild, `stop`
drains it, and `restart` composes stop and start. `stop --force` and `restart
--force` open a pidfd and validate the recorded process start token before
sending `SIGKILL` to that exact process descriptor. If the runtime's SIGHUP
source closes, the daemon disables only the reload branch after one diagnostic;
it does not spin on an always-ready closed stream. SIGTERM/SIGINT closes reload
admission and aborts any in-flight SIGHUP waiter before daemon shutdown, so a
stalled reload cannot retain the Profile lifecycle lock ahead of stop.
Daemon task failure still drains reload, diagnostics and preset ownership before
returning the task error.
The `host start` launcher reserves the owner lease before spawning and passes
it to the child without releasing ownership. The child creates a new Unix
session before Host bootstrap, so
terminal process-group signals and hangup ownership do not remain shared with
the launcher. Foreground `host serve` deliberately keeps its caller's session.

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

The standard Agent preset also selects workspace and time context as ordinary
contributions. Workspace refresh and its last-good domain state commit together;
time context commits one UTC clock reading before each new provider retry series.
Both belong to the immutable Agent generation. Custom presets select their own
contributions through the Agent-only addon catalog. `rsi.tools.portable` is an
available Agent contributor that imports an explicitly configured Portable Tool
service into the same stage. It is enabled only by an explicit Profile leaf;
the default preset has no native dependency. Its wire and confinement rules
belong to [Tools](../../../rsi-tools/protocol/README.md#portable-contributions).
The global factory catalog also provides `rsi.ai.portable` for explicit
Language/Image provider composition. Its [provider contract](../../../rsi-ai/portable/README.md)
requires global drain/restart and does not change the default DeepSeek Responses path.

The standard preset selects [plan policy](../../../rsi-agent/plan-policy/README.md)
through that same catalog. Planning starts disabled and can change through the
shared Session command service before or after publication. Its Tool allowlist
adds a constraint to existing approval and sandbox policy.

The standard [repeat reminder](../../../rsi-agent/repeat-tool-reminder/README.md)
adds source-attributed advice after repeated identical settled Tool calls.
Its bounded domain cursor and advice commit together; inspecting history never
replays the heuristic.

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

On Unix, `rsi addon list [--root ABSOLUTE] [--output text|json]` reads installed
and separately enabled artifact identities. `rsi addon install MANIFEST`,
`enable ID`, `disable ID` and `uninstall ID` accept the same options. The default
source root is `<config>/native-addons`; an explicit root manages that store only.
The product resolves its authorized first-component OS alias before no-follow
root acquisition. Relative manifest paths are resolved from the current directory;
source acquisition still rejects links and unresolved parent traversal.

Installation copies bounded bytes without executing or enabling them. Enable
selects the latest installed artifact for the current target; reinstalling an
enabled ID preserves its prior enabled digest until another explicit enable.
Uninstall requires disabling first and retains immutable source objects. No command
opens the Loader cache, starts a Service Host or creates a Session. Source mutation
receipts describe the atomic index publication and its directory-sync result;
they do not claim runtime staging or Agent generation application. JSON revisions
use canonical decimal strings. A running standard Host observes its enabled store
through the existing native manager; `rsi --profile inspector native` reads that
manager's separate status and retained resources.

Standard workspace skills use project definitions by default before the RSI config
`skills` directory and then the optional captured HOME's `.agents/skills`.
The launcher captures HOME explicitly; `StandardComposition` never reads it.
An absent HOME with complete XDG paths omits that optional root. Remote clients
use the owning Host's skill catalog, not their local filesystem. Explicit Profile
workspace-context configuration retains control of its user roots.
