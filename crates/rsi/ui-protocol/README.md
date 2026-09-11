# rsi-ui-protocol

Renderer-neutral UI data has a named schema, explicit schema version, a renderer
identity, bounded JSON model, displayed actions and source descriptors. A model
may include a standard declarative view for renderers that support those elements.
The model never chooses an executable module URL. Renderer manifests are admitted
separately by the presentation owner. Nominal names contain 1–128 ASCII letters,
digits, dots, underscores or dashes; a leading dot is valid. Asset filenames
separately exclude a leading dot.
Declarative views, elements, bound views and surface descriptors reject unknown
wire fields, including nested elements of a model's standard view.

The wire carries exact application, target, contribution and presentation
identities plus a snapshot revision. These are staleness fences, not authentication
credentials or durable Fact cursors. Action references name only actions in that
snapshot; the owning registry checks membership before invocation. Source names
have meaning only in that same presentation.

`ExportScope` carries a bounded semantic kind and domain key. A product validates
that selector against actual caller authority and publishes an already narrowed
business client separately; the selector itself grants no access.

Each encoded model or snapshot is limited to 128 KiB, including JSON escaping and
its envelope. Actions carry at most 64 KiB. At most 32 actions and 32 source
descriptors occur in a model. This package validates data and owns no runtime,
Session, transport, terminal, DOM or plugin lifecycle. Owners separately reserve
aggregate snapshot capacity before asking a source to materialize a model.

`ModelSnapshot::write_json` validates identities, membership and nested action
inputs, then writes the complete envelope once through a 128 KiB bound. The
enclosing bound also covers its nested model and standard view. Callers reserve
storage before writing and discard partial output on error. Standalone `validate`
methods retain their complete semantic and encoded-size checks.
