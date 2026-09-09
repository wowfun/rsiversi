# rsi-session-files

This standard-product adapter exposes authenticated, finite workspace browsing
through ordinary endpoint and client plugins. The API consumes the independent
Files reader and the actual Session read-lease service. `files/open`, `read`,
`list` and `release` are version-one authenticated Data/Read operations: a lost
waiter cancels work. They accept a Session/Header target and relative paths or
retained file descriptors, never an absolute root or WorkspaceTrust override.

Every call decodes its closed bounded request, reserves materialization
scratch and validates ranges before acquiring the actual Session read lease. The Header's canonical cwd
selects the root. Trusted and Untrusted Session workspaces are equally browseable
by an authenticated caller; trust only governs instruction/skill promotion.
Session identity, Header fingerprint and file token are correlation values,
not secrets or authentication. Draft expiry is an unavailable domain object;
missing/revoked authentication remains an API authorization failure. API or
Session retirement and device revocation cancel the finite reader future.

Each endpoint generation supplies a distinct Files caller identity. Retirement drains endpoint
admission, then releases all tokens owned by that caller generation. Every token
continuation receives a freshly authorized binding and retains the finite Session
lease through I/O. Tokens never retain draft activity. Files owns retained native
handles and all page/snapshot/token/job limits; this adapter owns bounded wire
and Header materialization scratch. A response echoes its exact request. Clients
check that echo, file kind/length, exact byte offsets, byte counts, directory
ordering, descendant names and limits before publishing decoded values. The same
malformed-peer scenarios run through public client interfaces natively and in
real Dedicated Workers.

The client runs in native and Worker applications and owns no filesystem or
Session state machine. Dropping its future cancels the API read. It never retries
a read/open/release automatically; unavailable/changed objects require explicit
refresh. HTTP authentication probes use an isolated loopback server and test-only
credentials; actual draft lifetime is tested at the Session owner and through
standard-product integration.
