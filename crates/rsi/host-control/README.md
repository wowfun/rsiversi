# rsi-host-control

The product management protocol carries explicit reload and configuration-watch
leases over the authenticated native `manage` sibling stream. It is separate
from the frozen Lifecycle v1 stop/status protocol. Every request carries the
active Host epoch and generated interface descriptor. Frames use a big-endian
32-bit byte length followed by strict JSON, bounded to 256 KiB before allocation.

A watch request acknowledges one observation lease and then retains it until
EOF, unexpected input or Host shutdown. It neither changes the Profile nor
authorizes source compilation. Multiple clients share the existing Profile and
native-addon observation policy; only the last lease disables automatic work.
Reload returns the actual bounded convergence outcome. Disconnecting a reload
waiter does not cancel an already admitted update.

Source-build requests carry an explicitly trusted, frozen recipe and the caller's
scrubbed environment. They own a connection lease, separate from configuration
observation. Identical recipes share one build loop; overlapping output directories
are refused. The last disconnect cancels and joins the managed process tree.
The initial and subsequent replies report building, publication or categorical
failure. Output and environment values never enter these diagnostics.

Environment comparison uses a Host-epoch-salted fingerprint of the frozen
scrubbed tool environment and allowlisted credential fallbacks. The endpoint
returns only equality; it does not publish values or persist fingerprints.
Clients warn on a difference and retain the active Host's environment. Changing
durable Credentials remains independent of process environment.
