# rsi-browser

Browser owns one generation-bound runtime lease, its frozen destination policy,
private CDP and MCP connections, deterministic checker and user evidence. Process
owns native launch and reaping; Sandbox supplies the exact restricted plan.
Neither Jobs nor the global MCP manifest participates.

Restricted runtime requires verified Bubblewrap, Chromium sandbox and cgroup v2.
PID/network/mount namespaces expose only fixed read-only runtime resources and
private scratch. There is no host network route. An owned bounded broker is the
sole HTTP/CONNECT exit, validates complete DNS answers and pins actual addresses.
Loopback is production-denied. Chromium CDP uses a pipe; no CDP TCP listener is
present in the page namespace. Separate restricted client scopes attach through
private authenticated bridges. Browser and client epochs never cross attempts.

Fixed Node, Chromium and dependency paths/digests are operator-supplied. Checker
pins Playwright 1.63.0; exploration pins @playwright/mcp 0.0.80. No download or
unconfined fallback occurs at activation. Each isolated process scope defaults to 1 GiB memory, 256 processes and a
ten-minute runtime maximum; Browser and its client occupy separate scopes. A missing enforcement mechanism
is NotReady, not an invitation to weaken the plan.

Preparation hashes the installed runtime on a blocking worker, then proves one
launch/retirement before publishing readiness. Operators keep those installed
bytes unchanged for this runtime generation; replacing them requires a new
generation and preparation. Opens consume the verified generation without
rehashing it. Preparation is serialized and idempotent for a verified generation,
so occupied slots cannot turn readiness into a verification failure. A failed process settlement fences the runtime until replacement
and reports that cause rather than a temporary capacity refusal.

Checker predicates include an entry identity guard, final URL, visible text and
role/name visibility. Missing entry identity and protected pages are unavailable
targets. Only complete assertion results settle a verdict. Navigation is bounded
to 20 seconds, checker to two minutes; dialogs are dismissed and recorded.
Exploration exposes only fixed navigate and text observation wrappers. Raw MCP
catalogs are never exposed. Screenshots are viewport-only user evidence, not
Agent Media: at most four 512 KiB canonical PNGs. No screenshot base64 enters
durable Agent facts.
Checker operations share a 110-second budget, including evidence capture,
inside the Rust two-minute exchange deadline. Navigation and subsequent text
share one 20-second budget inside their 25-second exchange deadline.
Exploration validates the current top-level URL from private structured CDP
metadata after every operation; page text never supplies a policy coordinate.
Multiple page targets fail closed. Structured control replies and private MCP
replies have separate bounded receive lanes; the idle MCP reader cannot consume
or block a policy response. A blocked checker destination retains its URL
and blocked disposition but withholds snapshot, assertion details and images.

Retirement stops admission/egress, drains clients, terminates the restricted
scope, awaits Process settlement, then removes owned scratch. Cleanup failure
retains capacity. Abrupt Host death has a bounded final retirement guarantee
only after native proof.
All bounded response queues interrupt delivery when their owner stops, including
error delivery, so an unread saturated queue cannot prevent retirement.
Any command timeout or abandoned command retires its scope before another command
can enter. Late responses cannot satisfy a later request. Explicit `close()` awaits
settlement; dropping the last public owner starts retirement, whose independent
guardian retains capacity until both processes settle.
Runtime hashing covers npm command links under `node_modules/.bin` too. Only
relative links resolving to regular files within the hashed tree are allowed.
The checker uses the stable Playwright dependency; upstream MCP independently
requires its pinned 1.63.0-alpha-2026-08-31 Playwright core. Both trees are covered
by the artifact digest; merging them would change the upstream MCP contract.

Default suites are deterministic and keyless. Linux product CI additionally
installs the pinned Node/Playwright runtime, prepares a systemd user manager and
explicitly runs the native checker/retirement and abrupt-owner-death tests.
The [CI user-manager fixture](../../../fixtures/rsi/browser-runtime/README.md)
records existing runner state and restores only job-created resources on exit.
These tests remain opt-in locally and require the three `RSI_TEST_BROWSER_*`
runtime paths. Real provider tests stay separate and require explicit credentials.
The abrupt-owner proof observes renderer filter initialization within one shared
two-second deadline: a visible renderer command line alone is not readiness.
Every observed renderer must reach `Seccomp=2` and `NoNewPrivs=1`; namespace and
no-sandbox-flag checks remain mandatory. A persistent missing filter fails with
the observed status rather than being skipped.

Initialization waits for the browser helper's ready acknowledgement before
starting the CDP client. Client CDP traffic cannot precede Chromium's pipe
installation. The helper assembles NUL-delimited CDP frames in a reusable 8 MiB
buffer, copying incoming fragments once and bounding each complete frame.
