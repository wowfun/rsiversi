Read the [family contract](README.md) before changing human-question behavior.

- Keep request and answer bounds at the protocol/service boundary. The protocol
  owns neither terminal input nor Agent state.
- Human waiters are synchronous and cancellation-owned. Host restart must not
  reconstruct a waiter from historical Tool data.
