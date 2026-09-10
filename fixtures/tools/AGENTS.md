Shared fixture tooling owns build and browser orchestration only. Consumers own
scenario semantics, dependency locks and result assertions. Serve explicit asset
allowlists on ephemeral loopback ports, bound subprocess and Worker lifetimes,
and always close browsers and listeners after failure.
