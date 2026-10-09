# rsi-projection

`rsi-projection` is an ordinary plugin for named, process-local pure JSON
projections. Consumers register units with generation-owned leases and compute
deterministic derived views from one input snapshot.
Withdrawal removes visibility under the registry lock and destroys the removed
unit after unlocking, including any dependent registration leases it owns.
Meta withdrawal removes service discovery; escaped typed projection handles
remain usable while their unit registration leases remain alive, following
Meta's [local-service lifetime](../rsi-meta/core/README.md).

Projection output and any future cache are disposable replay shortcuts. They
never replace Agent facts, Settings, Workspace records, or another durable
authority.
