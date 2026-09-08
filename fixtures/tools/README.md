# Fixture browser tooling

The shared Worker runner builds an explicitly selected locked Rust fixture,
checks its browser dependency closure, generates matching WASM bindings and
serves only that fixture's declared assets on an ephemeral loopback port. Each
consumer owns its cases, Worker entry point and pinned browser dependencies.
It reports actual engine versions and Worker results, without simulating an
executor or treating compilation as browser execution evidence.

The build helper can also supply bounded glue/WASM artifacts to an actual API
listener fixture. Such a consumer owns its bootstrap document and network cases;
the helper does not intercept API traffic or provide a second HTTP implementation.
