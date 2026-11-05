Read the family [contract](README.md) before changing extension integration.

- Core owns framed process I/O and lifetime only; keep language and Agent semantics in adapters.
- A process response is not canonical publication. Retain exact operation resources until the owning product confirms or reconciles its receipt; never replay to resolve uncertainty.
- Treat imported code as trusted native-equivalent code. State I/O bounds and watchdog behavior without claiming CPU, memory or filesystem confinement.
- Default transport and journal tests are deterministic. Native runtimes, installed third-party packages and providers require explicit opt-in.
