# rsi

This package implements the standard RSIversi product described by the product
[contract](../README.md). The library owns the explicit linked factory catalog,
standard composition, product-owned Profile catalogs, and construction of the
transport-independent `SessionApplication`. The binary owns command-line
parsing, Session/headless orchestration, terminal input and rendering, process
signals, and the Tokio runtime. There is no parallel library-owned headless
runner.

The standard catalog links providers but does not select or enable a deployment.
A persistent Profile instantiates the intended provider, while Settings names
the exact default deployment and model. Tests can inject a credential store at
the public composition seam without consulting real user state.

`rsi --profile NAME [application arguments]` selects one named Application
Profile. The built-in `headless` application accepts one positional task or
`--stdin`; `session` is the line-oriented interactive application. Both drive
the same `SessionApplication` surface, subscribe strictly after the durable
acceptance sequence, and render the subsequent live Facts. The acceptance and
terminal envelopes are binary-owned presentation records rather than a second
Agent execution API.

Application startup keeps the Agent-preset Settings host only when it becomes
part of an embedded Session Host. A compatible remote daemon needs the preset
catalog only to derive its launch preview, so the client shuts that Settings
host down before running the application command.

Exit status 0 means a completed turn; an interactive Session also treats its
user's locally cancelled turn as a successful control action. Status 1 covers
failures after the application has resolved its Session handle and entered
runtime work, including submission-time route or provider configuration
rejection and a failed, partial, interrupted, or budget-exceeded terminal
outcome. Status 2 covers command-line, profile/catalog, Host bootstrap, and
initial Session create/attach selection failures before that handoff. Status
130 means headless signal cancellation.

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
