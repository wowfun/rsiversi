# rsi-api

An ordinary Meta plugin supplies `ApiRegistrarContract` and `ApiDispatchContract`
from one registry generation. It owns registration retirement, independent
control/data/subscription admission, per-device limits and explicit-executor
mutation jobs. Protocols and adapters consume the [family contract](../README.md).

The factory accepts null or an empty object. Embedders can construct `ApiRegistry`
with an explicit `Execution`; this does not create a second composition runtime.
Plugin cleanup withdraws both supplies, fences all operations and awaits owned
work. Read futures and subscription streams hold admission until drop, terminal
result or retirement. Transport delivery leases retain quota independently of
domain work. Native panic reports an unknown outcome for a dispatched mutation
and a backend error for a read; a browser WASM trap remains whole-Worker failure.

`ConnectionApiFactory` independently owns the description, catalog and caller
registrations and publishes their exact `ConnectionDescriptionContract`. It
requires the registry and deployment/generation identities. Multiple listeners
consume this same generation; stopping one listener does not withdraw connection
operations. Its own retirement withdraws the description and drains all three
registrations. Embedders use `ConnectionApi::register` with the same explicit
identities and retain that owner independently of their transports.

Run `cargo test -p rsi-api -p rsi-api-protocol --all-targets` and the corresponding
Clippy targets. `cargo check -p rsi-api --target wasm32-unknown-unknown` verifies
the shared closure; actual Worker lifecycle verification belongs to the browser
fixture and application integration.
