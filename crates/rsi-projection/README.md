# rsi-projection

`rsi-projection` is an ordinary plugin for named, process-local pure JSON
projections. Consumers register units with generation-owned leases and compute
deterministic derived views from one input snapshot.

Projection output and any future cache are disposable replay shortcuts. They
never replace Agent facts, Settings, Workspace records, or another durable
authority.
