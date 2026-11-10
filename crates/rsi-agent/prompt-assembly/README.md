# rsi-agent-prompt-assembly

An ordinary Agent plugin implementing native prompt preparation through
[`PromptAssembler`](../composition-protocol/README.md#native-prompt-preparation).
It owns named sections, contexts and variables, rendering, Tool presentation order
and awaited presentation transforms. It owns neither durable writes nor Tool bodies.
The composition catalog isolates `PromptRegistryContract` per generation before
activation, so overlapping fresh and restored generations have distinct authoring
providers. The standard product declares this alongside the factory.

Registrations use the caller's Context or ScopedContext and the existing
`ScopedLayers` named storage. Nearer scopes shadow farther/global names; duplicate
names in one layer fail. Providers execute outside storage locks with the explicit
request Header, model, scope and cancellation. Registration removal is exact and
generation pins retain admitted work without a strong Runtime ownership cycle.
Callers retain the returned registration leases for the desired lifetime. Plugin
configuration retains them in its activation cleanup; passive entries add no child
Fiber or asynchronous change notification.
Literal declarations share immutable text in registry snapshots; rendering owns
only the request's resulting text. Rendering moves unchanged text into its result;
it does not clone the accepted presentation for literal or variable-free sections.

Sections sort by finite order then UTF-16 code-unit name order, matching DSH.
Contexts sort stably by finite order. Variables use `[a-z][a-z0-9_]*`; strict
`{{name}}` interpolation rejects malformed, unknown or unresolved values, preserves
a lone unclosed opener, and never rescans substituted values. Literal sections opt
out. Empty rendered sections disappear; nonempty sections join with blank lines.
Header, delegation persona and existing native producer IDs have stable default
positions preserving their original content and order.
Named sections `rsi.header` and `rsi.delegation` override those defaults through
the same scoped shadowing rules as other named sections.

One complete section replaces ordinary sections after presentation transforms;
multiple effective complete sections fail. This does not change Plan, approval,
sandbox or Tool-subset enforcement. Optional context suppression retains required
contexts. After an awaited presentation transform, admission refresh reevaluates
registered runtime contexts, replaces edits to their text, restores missing
required contexts and respects suppression. It does not rerun accepted section,
variable or Tool providers or presentation transforms. Transform-only contexts
retain their accepted text. Without transforms, native assembly samples contexts
once; callers can explicitly refresh an accepted presentation before rendering.
Rendered context enters the existing durable input path even with no other listeners. Runtime context is a
User snapshot with explicit `RuntimeContext` provenance. The executor compares
the rendered text with the last retained, entered snapshot; unchanged text emits
no claim. Clearing a previously entered snapshot emits the DSH cleared marker.
Only an entered input advances the projection, including through checkpoint
restore. Context retains a message position, not another copy of its text.

Transforms use an Agent-owned consuming continuation for awaited around execution;
they cannot mutate Tool definitions or create authority. Tool order has exactly one
`<unlisted-tools>` marker, no duplicate names, and validates named entries against
the pre-restriction universe. Known restricted names may be absent. Remaining admitted
names sort by UTF-16 code units at the marker. Omission uses that canonical name order.

Only resolved Step sections and entered runtime context persist. Temporary variables,
callbacks, schemas and registrations remain request/generation owned. Cancellation
does not replay provider effects. Mixed application and extension presets keep their
existing preparation owner; this package does not interleave native transforms with
receipt-bound JS callbacks.
