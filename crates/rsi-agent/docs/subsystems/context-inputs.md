# Context inputs

The [Agent Tools contribution](../../tools/README.md) consumes the read-only
`WorkspaceContext` Local contract for Markdown definitions. Workspace discovery
owns bounded filesystem observation and parsing; Tools owns catalog projection
and role resolution. Kernel receives the validated frozen role seed through the
Turn protocol and does not discover files. The workspace package's Kernel
dependency is test-only; production discovery depends on the narrow protocols.

The reference owner captures bounded conversation data and verifies immutable
CAS envelopes. Its model adapter uses the live Tool caller's Header; the product
supplies the actual target Header for human capture and admission. The Context
builder renders only the frozen preview and exact recorded read coordinates.
The mechanical Store owns atomic suffix horizons and pre-body byte bounds.

`rsi-agent-context` incrementally folds Facts into provider-neutral messages.
One claim reads a filtered horizon: all earlier turns and the claimed turn are
visible, while later accepted turns remain invisible even when the claimed
turn's own later executor Facts have higher session sequences. Store pages are
bounded by both Fact count and aggregate encoded bytes. Context folding
compacts complete oldest turns while pages arrive, inserts one deterministic
omission notice, and never retains complete lifetime history merely to project
its bounded tail. It never splits a tool call from its result. Workspace
contents are not implicit input: a model sees them only through an explicit
context source or Tool contract.

A claim carries the exact sequence of its own acceptance. The executor may
restore a session checkpoint only when that checkpoint ends before this
sequence; checkpoints that already folded the claimed or any later accepted
turn fall back to the canonical claim-filtered replay. Checkpoint maintenance
may still encode the complete durable tail for reuse by later claims. Its
optional writer keeps the latest request per Session and preserves FIFO across
distinct Sessions, so a hot Session cannot overwrite or starve another
Session's cache request.

Workspace instructions and skills are ordinary Agent execution contributions over
[a bounded filesystem source](../../workspace-context/README.md). The plugin owns
invocation interpretation, complete digests, last-good state and Session-bound
history cursors in its typed domain. The Kernel owns only generic role/source
validation, budgets and atomic Fact/control submission. The selected canonical
workspace supplies project sources for fresh, resumed and forked Sessions. The contributor runs before
each new provider retry series, while provider retries reuse the already entered
inputs. A successful Tool round therefore cannot hide workspace changes before
the next model request; no Tool-specific filesystem-touch enumeration is needed.

Fork lineage and replay eligibility follow the [fork contract](turns-and-agent-trees.md).
Context consumes its validated prefix rather than selecting another boundary.
