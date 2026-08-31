# rsi

The `rsi` product is the standard RSIversi application composition. Its Rust
package library owns the explicit linked factory catalog and Base/Headless
Profile fragments; its binary owns CLI parsing and construction of the Tokio
runtime.

The standard catalog links OpenAI, OpenAI-compatible, and DeepSeek factories
without implicitly enabling a deployment. A persistent Profile instantiates
the chosen provider and Settings names an exact default deployment/model.

`rsi run` submits exactly one user turn from either one positional argument or
`--stdin`. It owns the documented fresh/resume workspace binding, store-root
writer lease, text or versioned JSONL output,
bounded cancellation and flush, and process exit codes. Web, remote control,
Media export, and native package management are not part of this product.
If the Agent Store permanently rejects a pending prefix, the attached run
returns a flush-class Run error instead of waiting for an unreachable terminal.

The Rust library additionally exposes direct Image turns. A direct Image turn
validates its exact Image route and does not require the session's default
Language deployment to be available. Image results remain Media references.

The product materializes its built-in `standard` Agent preset as a verified,
digest-addressed cache asset and prepends it before configured and writable
user roots. Unix materialization creates, verifies, and publishes through
no-follow directory descriptors; the portable fallback rejects observed link
or reparse-point components before publishing. Each fresh session consumes a process-local draft carrying the
current preset generation; durable resume uses the Header's required
`agent_preset_id` and cannot override it. The Kernel retains that exact pin for
the resident session, while the executor reads definitions and executes every
Tool through the claim's immutable catalog. The runner prepares that exact
fresh or resume generation before any durable Workspace registration, and a
generation-preparation failure therefore cannot create a Workspace row.
Dropping an unsubmitted resume token has no Store or resident-capacity side
effect.

On Linux, the binary resolves its own canonical executable and `/bin/bash`,
freezes the scrubbed child environment before Host construction, and passes
those values explicitly into the standard composition. The Bash Job producer
is global because Jobs identities outlive Agent generations. The model-facing
`bash`, three Jobs controls, and `apply_patch` are separate Agent-only
contributions activated inside an unpublished Tool catalog and atomically
sealed with one preset generation. Other platforms omit the Linux-only Bash
and apply-patch contributions before Runtime mutation rather than advertising
effects whose native lifecycle guarantees were not tested.
The Headless Jobs finalizer cancels and reports all unfinished turn work before
the terminal Fact; unreported background completion blocks a successful turn.
These closure claims require the host process to remain alive through
finalization. Restricted standard plans also bind Bubblewrap to parent death;
`danger-full-access` has only process-group ownership and cannot honestly claim
cleanup after host `SIGKILL` or containment of a descendant that calls
`setsid(2)`.
