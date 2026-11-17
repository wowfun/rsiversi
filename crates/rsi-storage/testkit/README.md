# rsi-storage-testkit

This test library provides an in-memory KvBackend for consumer tests.
Use the production DomainFactory for typed records, updates, subscriptions and
close behavior. Fault controls pause before or after commit, reject known
writes, or report an uncertain outcome. They do not emulate native filesystem
durability. Raw fixture seeding intentionally permits business-codec failures.

BackendFactory registers an explicitly supplied backend under one test
name; its cleanup withdraws that registration. Tests own the lifetime and release
of any injected gates and drain accepted Domain operations before shutdown.
