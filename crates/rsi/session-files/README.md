# rsi-session-files

This standard-product adapter exposes authenticated, finite workspace browsing
through ordinary endpoint and client plugins. The API consumes the execution
resolver and the actual Session read-lease service. `files/open`, `read`,
`list` and `release` are version-one authenticated Data/Read operations: a lost
waiter cancels work. They accept a Session/Header target and relative paths or
retained file descriptors, never an absolute root override.

The endpoint plugin also publishes the same typed client for trusted embedded
applications. Its private adapter admits only these four exact read operations
with Local origin through the same registry, byte pools, decoders and Session
leases. It inherits the endpoint generation's retirement. Remote applications
obtain this capability from the ordinary authenticated domain client plugin.

Every call decodes its closed bounded request, reserves materialization
scratch and validates ranges before acquiring the actual Session read lease. The Header's canonical cwd
selects the root for an authenticated caller. Each call forwards its actual
ingress origin to Session admission and selects a fresh execution lease for the
Header location; remote paths never enter the Service's native Files reader.
Session identity, Header fingerprint and file token are correlation values,
not secrets or authentication. Draft expiry is an unavailable domain object;
missing/revoked authentication remains an API authorization failure. API or
Session retirement and device revocation cancel the finite reader future.

Each endpoint generation supplies a distinct Files caller identity and retains at
most 64 token resources, including opens in progress. Each token pins its original
provider and is bound to the opening principal and exact Session/Header. A fresh
view authorizes continuation without borrowing the opening caller's old grant.
Reconnection cannot transfer a token to another provider. Release and endpoint
retirement dispose the retained scope; idle tokens retain no Session activity.
Release authenticates the owning principal and exact token target but needs no
new execution lease or live Session; it remains cleanup after Use withdrawal,
target disconnection or draft expiry.
Retirement drains endpoint admission, then releases all retained resources. Every token
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

Version-2 Files operations include the opened object's executable flag. Metadata
and bytes retain one source version; clients cannot supply a different mode while
continuing an existing token.
