# rsi-ssh-protocol

Connection coordinates contain only a bounded DNS name or IP address, an explicit
nonzero port and a bounded account name. SSH aliases, option syntax, URL syntax,
control characters and arbitrary configuration are not accepted. Parsing performs
no DNS lookup or filesystem access and confers no connection authority.

Pinned host keys retain canonical base64 SSH public-key wire bytes and expose a
SHA-256 fingerprint. Accepted forms are Ed25519, RSA (2048 through 8192 significant
modulus bits) and ECDSA nistp256/nistp384/nistp521. Lengths, SSH strings, matching
algorithm/curve names and complete wire consumption are checked before retention.
OpenSSH verifies the cryptographic signature during connection. RSA host signatures
use SHA-2. Unknown algorithms and certificates are unsupported; there is no trust
on first use or DNS-based trust fallback.

The helper wire uses a fixed 32-byte versioned header and fragments no larger than
16 KiB. Epoch, kind, flags, correlation/channel identity, sequence and body length
are validated before body allocation. A mismatched epoch never admits a body.
Ordinary request/reply messages can be fragmented; each reassembled message has a
2 MiB ceiling. Control request/reply frames are complete and at most 1 KiB.
Heartbeat, heartbeat acknowledgement and close have no body. Stream credit frames
carry one through four frame credits and a nonzero grant sequence; each data frame
consumes one credit independently of its byte length. This bounds tiny-fragment
queues as well as byte queues. Transport owners enforce aggregate budgets, ordered
message/channel sequences and reserved control scheduling; framing alone does not
establish those lifecycle or latency guarantees.

Reserved control payloads admit only terminate, PTY resize and Files
release. They reject unknown operations/fields and validate nonzero identities and
bounded dimensions at the transport boundary. This is transport admission, not
proof that the process or handle belongs to the caller.
Resize uses the Process owner's `PtySize` validation so reserved admission and
native resize cannot diverge on accepted dimensions.

Execution preparation uses normalized POSIX paths and the Sandbox/Process owners'
argument and environment budgets. Environment values and frame/reply payloads
are never included in Debug.
Prepared identities and stream coordinates are connection-local routing data; they
cannot reconstruct an Execution lease. Start options preserve the native 4 MiB
batch stdin bound; those bytes travel over an already reserved credited stream,
not inside a 2 MiB ordinary RPC. Output capture retains native byte coordinates so
a bounded tail that has advanced cannot be presented as complete lossless output.
Normal helper children reject notify/watchdog, DBus and SSH-agent environment keys;
the helper lifecycle notifier retains those capabilities separately.
SSH program environments also reject `LD_*`. These variables act on the native
dynamic loader before the Sandbox wrapper establishes confinement; explicit child
environment selection must not become a preload hook on that outer wrapper.

Initialization explicitly selects whether to expose the built-in `apply_patch`
`workspace_context` and `directory_picker` programs. These selectors are reserved and cannot be replaced
by the ordinary program map. Each enabled selector counts against the same
128-program bound and resolves to the leased helper image, with an empty child
environment.

Preparation carries an explicit source-reader choice, valid only for ReadOnly
pipes. This selects the pinned Sandbox view; a program name or marker does not
implicitly change confinement. The reply's enforcement evidence must confirm
read-only Host scratch and isolated networking.

`CACHE_CONTENTION_EXIT_CODE` is the shared process-exit contract for refused
cache-writer admission. Clients classify it only after the child is reaped;
other startup failures do not imply contention or authorize replay.
