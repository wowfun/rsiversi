# rsi-meta security boundary

Core trusts validated safe-Rust inputs and bounds every registry, frame,
channel, plugin configuration and overlay, activation, call, and shutdown
operation. Both input and normalized plugin configurations are checked before
the Runtime retains them. Service requirements and provisions use exact
contract identity and version. Context ownership and Fiber generation are
validated whenever a structural operation or service call crosses the runtime
seam. Independent Fiber reconciliation and live service calls have explicit
Runtime-wide concurrency limits; per-call channel limits are not treated as a
global memory bound. Provider closure and its admitted-callback count are one
atomic state, so an ordinary callback racing closure either commits its count
first or observes the closed gate. Cleanup-time calls from retiring dependents
remain ordered by the joined convergence transaction.

Native plugins are trusted process code. Hashing proves which top-level bytes were mapped; it does not sandbox them or recursively authenticate operating-system dynamic dependencies. A plugin can read memory, access the operating system, spawn threads, corrupt state, or abort. Only fully trusted artifacts belong in `NativeCatalog`.

`NativeCatalog` accepts a caller-selected source path; its cache directory is a
content-addressed staging destination, not a source allowlist. Possession of the
Loader service is therefore authority to select and execute trusted native code
with the host process's permissions. Products that need an artifact policy must
place it in front of that service rather than treating the cache as a sandbox.

The catalog rejects symlinks and special files and verifies a bounded regular
file. Unix maps a private unlinked copy of the verified bytes, so later
replacement or in-place mutation of the durable cache path cannot change the
staged content. The private descriptor remains writable to trusted code with
the host process's authority; this does not weaken the stated trust boundary,
because native plugins can already mutate arbitrary process resources. Windows
retains the verified cache handle with restrictive sharing; native Windows
behavior is not inferred from Unix evidence.

The ABI validates table sizes, versions, mandatory pointers, returned-buffer structure, output bounds, UTF-8/JSON at owning boundaries, and allocator ownership. An allocator-matched guard releases every plugin-returned buffer exactly once after either copying or rejection. Every unsafe operation in `rsi-meta-plugin` and `rsi-meta-loader` documents pointer, lifetime, serialization, or mapping requirements. Core contains no unsafe code.

A native callback that exceeds its deadline cannot be forcibly stopped safely.
The adapter-owned factory or instance gate remains held, is permanently
poisoned, marks the Runtime terminal, rejects new admission, and keeps the
instance and library alive until the foreign callback returns. Native
callbacks use dedicated operating-system threads so a permanently blocked
callback cannot exhaust Tokio's process-wide blocking pool. This deliberately
trades one thread per in-flight native callback for isolation; destruction is
offloaded for the same reason. Runtime-owned service callbacks are bounded by
`maximum_concurrent_service_calls` (1,024 by default), activation work by
`maximum_concurrent_reconciliations` (32 by default), and Loader initial
preflight by eight. Direct concurrent `NativeCatalog` or factory callers own
their own admission bound. Deployments must budget OS-thread stacks for the
configured concurrency; a callback that hangs can retain its thread forever.
The runtime's terminal fence bounds subsequent admission, but trusted native
code can still create arbitrary process resources.

The callback deadline watchdog is independent of the adapter future so caller
cancellation cannot disable terminalization. Once the callback publishes
completion, the watchdog retires immediately instead of retaining one Tokio
task and timer entry until the original deadline.
Process or Wasm adapters may offer stronger termination without changing core.
