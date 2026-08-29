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

The Rust library additionally exposes direct Image turns and trusted
process-local Job submission. A direct Image turn validates its exact Image
route and does not require the session's default Language deployment to be
available. Image results remain Media references, and the Headless Jobs
finalizer settles all unfinished work before the terminal Fact.
