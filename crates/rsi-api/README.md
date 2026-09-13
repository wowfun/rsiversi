# rsi-api

This family owns versioned domain API dispatch and transport resource contracts.
Domains retain their own operations, DTO validation and durable semantics;
transport adapters consume registered operations and authenticated caller
identity. The API foundation has no dependency on Session, Agent, Workspace,
Media, Settings, standard Host paths or rendering.

Immutable API buffers retain their byte reservation through every clone and
nonempty slice. Empty slices retain neither storage nor attached resource guards. Input retention, output retention and transport scratch use separate
budgets. These are encoded-byte bounds, not process RSS measurements. JSON is
parsed and encoded in Rust with the repository's exact-number policy.
An owner may attach an already acquired resource guard to immutable bytes with
`RetainedBytes::with_retention`. The guard follows clones, nonempty slices and
transport transfer without copying the payload or replacing its byte reservation.
This binds object-count admission to the same last-reader lifetime as its bytes;
the API foundation does not interpret the attached guard.

The registry is an ordinary Meta plugin. An operation's unique registration owns
admitted work. Retirement immediately fences new calls, cancels reads and streams,
and drains mutations before releasing the operation name for replacement. A
mutation becomes an executor-owned job when its admitted invocation is called,
even if its response future is never polled. A request still being received has
not started mutation work and must be cancelled when its registration retires.

Global admission has independent limits of 16 controls, 16 data calls and 64
subscriptions. An authenticated device can occupy at most 4, 4 and 16 respectively;
trusted local calls still obey global limits. Device counters exist only while
calls are admitted. The registry holds at most 2,048 operation registrations,
including retired registrations that still own work. There is no waiting queue.
Input and output each have separate retention pools: 2 MiB for controls and
64 MiB for each data/subscription lane. Finite
mutation response upper bounds are reserved before invoking a handler. Finite
reads reserve their measured encoding or copy size before allocating response
storage, under both the operation maximum and shared delivery pool. Read execution
scratch remains the domain adapter's responsibility; a small retained reply does
not require another full operation maximum to be available. Subscription
handlers reserve delivery bytes before allocating each encoded item. Domain
materialization and retained typed values have independent admission owned by
the domain; an already admitted immutable value may be measured before exact
wire allocation without reserving another operation maximum. Wire-buffer owners may outlive
call-slot release. Adapters retain a separate admission lease until response
delivery ends, including queued transport writes. This lease retains class and
device quota without retaining the domain handler or delaying its retirement.
Transport scratch and domain draft limits have their own owners.

Device authentication and local administration are separate capabilities. A
deployment owner supplies its persisted EndpointId under an exclusive owner lease.
The authentication provider retains at most 64 devices in one bounded non-session
Storage domain. It persists only endpoint-scoped SHA-256 verifiers of random
256-bit tokens and non-secret labels, never plaintext tokens. Credentials uses its
existing zeroizing secret wrapper; native client secret persistence remains with
the Credentials service. Successful durable revocation cancels existing read and
stream authority; already admitted mutations retain their work owner. Failed
publication preserves the last valid authentication state. Local administration
is not exposed remotely by possession of an ordinary device token. Registered
operation metadata declares either authenticated access or local-only access.
The registry rejects device origins for local-only operations before resource
admission; remote discovery omits those operations. Only trusted in-process
callers and the same-user, compatibility-checked local transport supply Local
origin. HTTP headers and request JSON cannot grant it.
Authenticated origins carry the verifier's revocation signal into registry-owned
work. Read and stream cancellation therefore does not depend on the HTTP consumer
polling another body item; mutation ownership remains independent of that signal
after execution starts.

An explicitly supplied API client can be exported through the ordinary
[Portable adapter](portable/README.md). Its closed wire vocabulary lives in this
family's protocol crate; semantic target narrowing stays with the supplying
domain. Portable transport carries operation identities and bounded bytes, never
caller-origin claims or Local trait objects, and inherits Meta's existing call
deadline and generation fencing.
