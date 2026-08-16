# rsi-meta-release-demo

`rsi-meta-release-demo` is the runnable thirteen-step scenario for the assembled product. It starts with no state directory and observes only public CLI, transport, file, package, and process behavior.

## Scenario

The scenario creates candidate locks offline, installs the initial workspace pair, starts the foreground daemon, attaches Unix and WebSocket clients at a gap-free cursor, activates and replaces providers while an old generation lease remains pinned, exchanges credit-bounded bidirectional streams, crashes after durable commit but before acknowledgement, and recovers the exact result by its original operation ID.

It then proves the process-fixed boundary separately: `apply` reports `restart_required` and exits 75 without stopping the daemon; `daemon stop` terminates the old process; `install` commits the pair offline; and a fresh `daemon serve` activates that pair once.

## Running the demonstration

```sh
cargo xtask rsi-meta release-demo
```

The product [testing policy](../../../crates/rsi-meta/docs/testing.md) and supported-platform CI use this as assembled release evidence, not as a substitute for focused unit, recovery, and failpoint tests. The command reports only the host platform it actually executes.
