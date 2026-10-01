# rsi-storage

This package defines the bounded JSON KV backend contract and provides the
ordinary `rsi.storage` hub plugin. A backend registration belongs to the
registering plugin generation and disappears when its lease is dropped.

The hub performs exact-name routing only. It does not open files, choose a
default backend, retry failed writes, or expose mutable registry internals.

Backend operations acquire one owned slot before dispatching blocking work;
cancelling a caller after dispatch does not release that slot. Disposal closes
admission, drains accepted work, then withdraws registration. A registration guard
closes admission even if its cleanup callback or future is dropped; the actual
blocking worker retains registration until it finishes. Old handles stay
unavailable. A replacement generation may open only after disposal completes.
The slot bounds dispatched blocking work, not caller-owned futures or payloads.
In-process callers bound their own outstanding requests; API owners apply their
admission quotas before invoking storage. Draining does not impose a syscall
termination deadline: releasing registration while an old worker can still write
would permit overlapping generations.

`KvBackend::ensure_available` is a local, non-I/O health check. `Io` means a
mutation is known not to have committed. `OutcomeUnknown` means commit cannot
be established; that backend generation is permanently fenced, including every
domain routed to it, and all later operations return `RecoveryRequired`.
Recovery recreates the backend, domain
facility and consumers from durable state; there is no automatic retry or reset.
Consumers retaining projections check health before admitting work or serving
cached state, including when the failed write belonged to another domain.
A [Domain authority](../domain/README.md) can additionally fence its own
publication task without fencing a healthy backend or its other domains.

Values have at most 64 levels, counting the root as level one, and at most
16 MiB of compact JSON. Encoding checks limits before growing its output;
existing out-of-contract durable values are corruption, without migration.
