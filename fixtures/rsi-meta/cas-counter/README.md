# rsi-meta-fixture-cas-counter

`rsi-meta-fixture-cas-counter` is a native black-box fixture for generation-pinned access to namespaced compare-and-swap state. It provides `fixture.cas-counter`, requires `state.cas` and `runtime.tick`, and performs get and compare-and-swap operations through public plugin frames.

The product conformance suite uses the fixture to observe namespace isolation, CAS winner and conflict behavior, frame routing, and restart persistence without reaching into private storage types.
