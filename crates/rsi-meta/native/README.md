# rsi-meta-native

`rsi-meta-native` defines native ABI v3 and its safe Rust authoring surface for
trusted in-process plugins. The maintained C11/C++17-compatible header is
[`include/rsi_meta_plugin.h`](include/rsi_meta_plugin.h).

## Version and exchange

Native ABI v3 uses `rsi_meta_plugin_entry_v3` and a single host/plugin exchange
port on 64-bit-pointer targets. ILP32 targets are rejected at compile time;
there is no untested alternate layout and no v2 compatibility path. The maintained
[`rsi_meta_plugin.h`](include/rsi_meta_plugin.h) is the authoritative contract
for entry and version rules, table and frame layouts, opcodes, statuses, and
every ownership and one-shot release transition. Native authors must compile
against that header rather than infer wire rules from this overview.

The exchange can describe identity, prepare one activation attempt, create and
destroy an instance, operate callback-lifetime caller and provider channels,
transfer capabilities, and manage setup effects. Native code does not publish a
static service descriptor. It provides services through the activation host.

## Messages and capabilities

The SDK presents calls as `Message` values containing bytes and owned,
transferable capabilities. Callback-scoped channel and effect handles carry
Rust borrow lifetimes, while transferable handles retain their native backing
until the owner releases them. The SDK keeps raw IDs and release tokens out of
plugin code. The header owns their exact validation, adoption, and cleanup
rules, including malformed and failure outputs.

Every successful preparation also declares `retained_bytes`: a conservative
charge for plugin-owned state kept behind the prepared capability until create
or release. The host cannot measure that native ownership, so it validates and
reserves the declaration before adopting the attempt. `Prepared::new` requires
the value explicitly; zero is correct only for truly empty retained state.

## Callback lifetime and effects

The safe surface exposes two callback-lifetime channel orientations:

- `Host::open` returns a `CallChannel<'callback>` for sending requests and
  receiving responses and the terminal outcome.
- `NativeInstance::serve` receives a `ProviderChannel<'callback>` for receiving
  requests, sending responses, and observing cancellation.

Neither channel can escape its callback lifetime. Activation similarly receives
an `EffectTxn<'callback>` for dynamic provide and deferred cleanup. Plugin code
requests commit after successful setup; errors, panics, and drops converge on
abort. The SDK maps those lifetime-bound operations to the raw exchange. The
header owns the exact channel, effect, callback-seal, and output state machines.

## Concurrency and trust

Callbacks may run on arbitrary host-owned OS threads. Factory preparation is
serialized per mapped factory, and instance callbacks are serialized per
instance while distinct instances may overlap. Admission is fail-fast, so
plugin code must handle rejected reentry and contention without relying on a
blocked callback.

Native plugins are trusted process code. A timeout poisons the adapter and
terminalizes the owning Runtime, but the thread, callback frame, library,
capabilities, and accounting remain retained until the callback returns.
Plugin-owned threads and transferable SDK handles must remain owned by plugin
lifecycle objects and return before successful finalization; both exchange
tables and the dynamic-library mapping may become invalid immediately after
that point. Leaking either across teardown violates the trusted native contract.
The SDK contains Rust panics at its trampolines, but it cannot contain aborts,
memory faults, data races, or arbitrary native behavior. The header owns the
raw destruction and finalization protocol; the
[`rsi-meta-native-loader` contract](../native-loader/README.md) owns host mapping, timeout,
cache, and teardown policy.

## Rust SDK

Implement `NativePlugin` and `NativeInstance`, then use
`export_plugin!(YourPlugin)`. The SDK exposes lifetime-bound `Host`,
`CallChannel`, `ProviderChannel`, and `EffectTxn` values plus explicitly owned
transferable capabilities. Factory identity and prepared injection are bounded
data; services are published dynamically through the activation host.

The SDK is an adapter for the header's lifetime model, not a second protocol.
