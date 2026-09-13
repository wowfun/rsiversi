---
name: Application plugins and a shared native and browser client foundation
---

## Problem

CLI, TUI, Web and future applications need independent Workspace, Models,
Settings, Media, Output and Session capabilities. A Session-owned application
root cannot express that composition. Separate native and browser business
controllers would also acquire different submission and observation semantics.
The foundation must support both environments without a second runtime authority.

## Decision

Applications are ordinary plugins. Session is one domain capability, the terminal
UI belongs to the terminal application, and the product server is the Service
Host. Protocols and stateless support remain libraries. The generic Meta and Host
products own no device, Workspace or Session policy.

One Runtime owns each composition. Its explicit Execution dependency supplies
scheduling, clocks and preparation; native and browser adapters retain the same
safe Rust contracts. Browser JS resources stay in bounded Worker-local owners,
with safe table identities and channels. Native files and immutable browser
bundles feed the same Profile compiler and Rhai evaluator. The
[Meta contract](../../../../crates/rsi-meta/README.md) owns that lifecycle.

Applications select connections and surfaces through ordinary ordered Profiles.
The launcher freezes and prepares their initial leaves before any backend starts.
A Session-free Shell owns real child Fibers with explicit Local isolation. Native
embedded services are child Profile generations; remote application exit detaches
from the independently owned server. The shared
[client controllers](../../../../crates/rsi/client/README.md) own Message
reconciliation and acknowledged observation cursors. Each renderer retains its
own bounded presentation state. Uncertain Web submissions remain immutable saved
requests across surface replacement; explicit retry resolves their original
identity through the shared controller. A fresh identity would describe another
message, not another attempt to learn the first message's outcome.
An explicit application interruption may stop client reconciliation while an
admitted server mutation remains owned. Otherwise an unavailable service would
prevent terminal shutdown indefinitely. This produces the original unknown
Message identity, never proof of non-execution. Dropping an ordinary submission
waiter still leaves reconciliation owned; the client contract distinguishes
explicit cancellation from waiter lifetime.

The [Session protocol](../../../../crates/rsi/session-protocol/README.md) is
separate from its native implementation. Models and completed Output are
independent capabilities. Registered Workspace identities resolve to the existing
canonical-cwd header when creating a Session; attach and history use Store truth.
Session and Store formats remain unchanged, and existing headers retain their
original settings and composition pins. A single Session plugin owns bounded
live draft preparation across local, UDS and HTTP adapters.

The [API foundation](../../../../crates/rsi-api/README.md) owns versioned operation
registration, negotiated connections, admission and transport buffers. Mutations
transfer ownership before awaiting replies; a disconnected waiter cannot abandon
accepted server work. Domains retain their own receipt authority. Device
credentials are registered and revoked through an explicit same-user operator
application, while remote clients receive only authenticated public operations.

The [Service Host](../../../../crates/rsi/service-host/README.md) owns native
process exclusivity. EndpointId persists, HostEpoch identifies the running
generation, and HostLaunchKey retains the exact executable gate for automatic
local reuse. Explicit remote clients negotiate wire and domain versions instead.
The existing `session-host` lock namespace remains so a renamed binary cannot
bypass a live owner. Host Profile launch hashing retains the
`rsi.session-host.launch-key.v1` separator. The separate local API compatibility
key hashes that launch key and the product build under `rsi.local.api.v1`;
it gates this transport generation and does not replace process ownership.

The [Web application](../../../../crates/rsi/web/README.md) runs Meta, Profiles,
shared controllers and two independent surfaces in a Dedicated Worker. Its
document bridge renders views and forwards input. Assets are a separate plugin
consumed by the Serve application. Production uses TLS with HTTP/2: actual
browser tests show that idle HTTP/1 subscriptions can exhaust the browser pool
and block controls despite independent API admission. HTTP/2 idle sockets also
need explicit ownership after a rejected request: stream admission alone cannot
bound a connection with no remaining stream. The HTTP adapter retains idle
admission and an idle deadline while active subscriptions retain delivery owners.
Explicit loopback HTTP
remains a transport debugging mode.

## Alternatives considered

Keeping applications below Session couples independent capabilities to a
conversation. Moving only the TUI directory leaves that ownership unchanged.
A second JavaScript business runtime, Cordis browser replacement, or native
sidecar substitutes another authority for the selected shared Rust foundation.
A universal rewrite of native Tokio users adds work outside the browser closure.

The earlier local-only HTTP rejection addressed a narrower product scope.
Multi-device access requires explicit authentication, resource admission and TLS.
Executable equality cannot serve as a cross-device wire version. A generic
durable request-outcome ledger would duplicate domain truth; the clients use
Message status and exact live interaction receipts instead.

Media upload independently publishes an immutable object. Rolling it back when
a later Message fails could break another Session. The descriptor remains usable
for reconciliation; cross-service staging, reference tracking and garbage
collection require a separate recovery protocol and remain a future milestone.

## Consequences

Finite API reads must not monopolize receiving capacity while awaiting their
headers. Session history can declare the entire Data ceiling even for a small
page, otherwise rejecting a concurrent explicit Goal mutation before dispatch.
The shared client admits reads by validated response length, with a full-ceiling
fallback for unknown-length bodies, while mutations reserve before exchange.
This keeps the existing byte bounds and unknown-outcome semantics without
replaying controls or raising connection budgets.
The HTTP owner explicitly declares finite wire lengths. Its delivery frame
wrapper cannot preserve a generic body's size hint, so relying on the HTTP codec
to infer Content-Length would send these reads back through full-ceiling
fallback on HTTP/2. Explicit lengths retain the existing delivery guards.

The terminal, Serve and Web applications share controllers and independent domain
contracts. Profile replacement is ordinary graph convergence. Existing local and
UDS model clients observe a changed provider Profile without acquiring a new
connection identity. Retained durable headers can be displayed after Workspace
and provider configuration disappear. Desktop, mobile and floating applications
remain future consumers of these boundaries.

Browser synchronous preparation cannot be preempted; deadlines are checked again
after it returns. A WASM trap fails the entire Worker and is not clean Rust
shutdown. Plugin cleanup drains pending connection negotiation as well as active
requests. A browser may physically retry a POST before receiving headers even
when the application invoked Fetch once; application-level no-replay is not
transport-level at-most-once delivery.

Remote negotiation permits heterogeneous builds and therefore provides a weaker
artifact check than local executable equality. Strict DTO admission and common
adapter scenarios constrain that boundary. Buffer budgets bound retained encoded
application data, not total RSS. A bounded history page can begin inside streamed
output, so the Web view marks its missing prefix explicitly.

Repository-owned native conformance and real Chromium/Firefox Worker fixtures
cover lifecycle, ABI, malformed input, backpressure, revocation and uncertain
outcomes. The product browser fixture drives the actual TLS/H2 application;
Linux PTY tests exercise the native terminal. Live coding validation uses an
external immutable oracle and separates provider capability from fixture checks.
Native Windows/macOS behavior requires those environments and is not established
by Linux or browser portability tests.
