# rsi

This package is the standard RSIversi application composition. The library
owns the explicit linked factory catalog, Base/Headless Profile fragments, and
the public Language/Image Headless runner. The binary owns command-line parsing,
standard input, signal handling, output, and the Tokio runtime.

The standard catalog links providers but does not select or enable a deployment.
A persistent Profile instantiates the intended provider, while Settings names
the exact default deployment and model. Tests can inject a credential store at
the public composition seam without consulting real user state.

`rsi run` accepts one positional prompt or `--stdin`, creates or resumes one
durable session, and emits text or versioned JSONL. Raw Facts are flushed as
they are published and include the durable prefix known at publication time;
the terminal outcome is emitted only after its Fact prefix is durable. SIGINT
requests bounded cancellation and exits with status 130 after the terminal
prefix is durable. A fresh session accepts `--agent-preset ID`; when omitted,
the current `rsi.agent-presets` default is resolved before the durable header is
created. The selected id is frozen in that header, so resume rejects
`--agent-preset` and keeps the session's original generation identity. Fresh
generation construction and resume generation preparation both complete before
the runner durably registers the canonical Workspace. Resume preparation
returns the authoritative Header and resident-or-current pin as one move-only
token consumed by submission; generation-preparation failure cannot leave an
unrelated Workspace row.

The library's direct Image turn surface persists request/intent/start, imports
each output through Media, and renders only `media:<MediaId>` references.
Headless maps exact Agent session/turn identities into generic Jobs owner
scopes. Its pre-terminal finalizer cancels and boundedly joins only work owned
by that turn; unscoped process work and concurrent turns are unaffected. A
scope timeout becomes the sole durable failed turn outcome while the still-live
trusted future remains tracked and that exact scope stays closed.

`rsi agent-preset` is a management-only command family. It discovers presets
through system roots supplied by the product, then the absolute configured
`roots` in the independent `rsi.agent-presets` Settings namespace, then the
writable `<config>/agent-presets` root. The same namespace layers an optional
user `default` over the fixed deployment default `standard`; each configured
root carries an independent `system` or `user` trust label but remains
read-only. `default set` persists any syntactically valid id; current discovery,
health, and composition are checked only when a fresh session resolves that
selection. `roots` is an order-preserving array of at most 32 `{path, trust}`
objects; `path` must already be absolute, `trust` defaults to `user`, and paths
are not shell-expanded. Management commands emit text or one JSON document and
use only exit statuses 0 and 2. They do not start, switch, or compose an Agent
generation. Copy names both identities explicitly as `copy --from SOURCE --id
ID [--name NAME]`. JSON roster and show rows always expose `id`, a `metadata`
object containing `name` and `description`, independent `source` and `trust`,
flat `status` and nullable `reason`, and `default`.

The shipped `standard` asset is materialized below a digest-addressed,
owner-writable cache, but the catalog grants System authority only to that
exact verified `standard` identity and directory. Other cache siblings are not
discovered as presets and cannot inherit System source or trust.

Exit status 0 means a complete turn, 1 means a runtime or terminal turn failure,
2 means command-line or boot failure, and 130 means signal cancellation.
