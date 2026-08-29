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
prefix is durable.

The library's direct Image turn surface persists request/intent/start, imports
each output through Media, and renders only `media:<MediaId>` references.
Headless maps exact Agent session/turn identities into generic Jobs owner
scopes. Its pre-terminal finalizer cancels and boundedly joins only work owned
by that turn; unscoped process work and concurrent turns are unaffected. A
scope timeout becomes the sole durable failed turn outcome while the still-live
trusted future remains tracked and that exact scope stays closed.

Exit status 0 means a complete turn, 1 means a runtime or terminal turn failure,
2 means command-line or boot failure, and 130 means signal cancellation.
