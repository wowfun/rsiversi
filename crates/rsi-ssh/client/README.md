# rsi-ssh-client

The native Linux client writes one private generated OpenSSH configuration and known-hosts
file. Invocation always supplies `-F` and its exact synthetic host entry, disables
connection sharing, agent and socket forwarding, user commands, password and
interactive authentication, host-key updates and DNS trust. The pinned public key
and explicit identity path are its only authentication inputs. No raw OpenSSH
option or alias is accepted from target data.

Identity paths must be absolute UTF-8 paths without OpenSSH percent or environment
expansion syntax. Quoting preserves literal spaces, quotes and backslashes. The
configuration directory remains owned until the prepared invocation is dropped;
callers retain it through the actual SSH child lifetime. Child environment is
explicitly empty; it does not inherit SSH_AUTH_SOCK, proxy, askpass or agent state.
The remote command is supplied only by the trusted transport owner, not target
configuration or model arguments. This library does not grant permission to run it.

Pure tests cover rejected inputs and generated configuration. Opt-in Linux tests
use an isolated loopback OpenSSH server with temporary identities and hostile
user and system configuration in a Bubblewrap namespace. They require explicit
`RSI_TEST_SSHD`, `RSI_TEST_SSH_SESSION`, `RSI_TEST_SSH_AUTH`, `RSI_TEST_SSH_USER`
and `RSI_TEST_SSH_FIXTURES` paths/account; the fixture directory must be under a
private home subtree compatible with server StrictModes. An optional
`RSI_TEST_SSH_LIBRARY_PATH` supplies privately extracted server dependencies.
They do not exercise a production user's SSH setup.

The process client consumes an explicitly supplied client-role mux connection.
It prepares one-use connection-bound plans, registers streams before start and
retains accepted creation through reply publication. Abandoned creation terminates
and settles its target process. Captured tails retain absolute offsets and gaps;
lossless ports preserve ordered bytes and explicit errors. Native input ACKs, not
credit grants, establish accepted prefixes. Malformed or missing effect replies
produce OutcomeUnknown and retire the connection; no effect is replayed.
Status uses a five-second bounded long poll; settlement wakes it immediately.
Ordinary process cleanup shares the existing monitor. A failed Start without a
published process owner uses its own settlement query. Output remains independently credited.
Native settlement is reported separately from lossless-output completion. Saturated
output cancellation can therefore report successful reaping while reads retain
their explicit interruption error. Cleanup waits for reserved admission rather
than interpreting a full control queue as a broken connection. Only undispatched
capacity rejection and read-only status are retried; uncertain effects are not.
Discarding an unstarted plan retires its unused local streams before releasing the
target reservation. A started process retires abandoned local output only after
native settlement, then releases the target slots. This ordering prevents new
target generations from overtaking local receipt of a cleanup reply.
The managed connector retains the actual SSH child and private configuration until
reaping, and sends fresh heartbeats every two seconds. Each process connection,
prepared plan and accepted operation retains that owner. Last-owner release or
explicit shutdown closes the mux, allows three seconds for orderly target exit,
then kills and reaps SSH. Losing SSH is not evidence of remote process settlement;
the independently verified target cgroup and watchdog enforce that boundary.
Grants and Execution leases remain supplied by product owners.

Targets require the fixed `/usr/bin` coreutils paths used by bootstrap,
`/bin/sh`, `/usr/bin/systemd-run`, `/usr/bin/systemctl`, `/usr/bin/getent`,
and `/usr/bin/bwrap`. Systems without those entry points, including an unadapted
NixOS installation, are unsupported and fail before target execution.

Bootstrap accepts a bounded locally verified same-CPU Linux helper artifact and a
fixed command containing only validated hexadecimal identifiers and decimal bounds.
It uploads exactly the declared byte length into a private target runtime staging
directory, verifies SHA-256 before execution, then invokes the cache installer.
An observed SSH exit code 75 during initialization reports CacheContentionTimeout.
Signals, other codes and missing exit status remain general initialization failures.
Reaping success is recorded separately from the exit status. This diagnostic never
permits automatic replay of bootstrap or an accepted remote unit.
The shell removes only its own staging directory. No forwarded sockets, ambient
SSH configuration, user-provided shell fragment or automatic replay participates.
Connect publication is retained independently of its waiter; unpublished owners
close and reap. Stderr is drained with a fixed budget and never exposed as an error
containing identities or server-supplied text. Bootstrap and initialization have a
45-second deadline. Startup failure classification waits at most five seconds
for the supervisor's reap receipt, preserving the original error without that
evidence. The supervisor retains child cleanup ownership after this wait expires;
only an actual reaped contention exit can change the classification. Only the target's advertised digest completes initialization.

The Files proxy records the exact opaque caller/binding with each remote token.
Foreign bindings reject before a request is sent. Open publication retains cleanup
after waiter loss, and reply pages are checked by the Files protocol owner before
publication. Local token release withdraws access synchronously and retains the
reserved remote cleanup request separately. Disconnect never substitutes local I/O.

`execution_provider` freezes this process/Files connection into one SSH Execution
backend. It preserves target revision, connection epoch and an opaque Sandbox read
scope generation; all path and program resolution uses target RPCs. Native process
settlement retains the caller's exact Execution pin independently of handle or
waiter lifetime. It cannot reconnect, substitute another target or select a local
capability. Product composition supplies the live admission grant before leasing it.

Product-confirmed identity bytes may be copied into a private 0600 file retained by
`PreparedSsh`; mutation of the source path cannot change that prepared connection.
The source trust decision and digest remain product-owned. Helper validation requires
a bounded same-CPU ELF64 Linux executable with System V or Linux OS ABI and
without `PT_INTERP`; a matching digest
for a dynamically linked image is insufficient for the static helper contract.

The direct PTY connection handle can await cancellation acknowledgement separately
from process exit/output settlement. Cancellation is one retained control operation;
dropping an acknowledgement waiter neither cancels nor replays it. A successful ACK
means the target accepted termination, not that descendants have already exited.
This distinction permits measuring the control lane while output credits are full.

Termination cleanup retries temporary control-lane capacity within 30 seconds.
Capacity retries for termination and release back off from 1 ms to a 100 ms cap;
successful or uncertain effect replies are never retried. Existing cleanup
deadlines include the backoff waits.
Expiry retires the connection epoch and reports an unknown outcome; it does not
replay the accepted effect or claim remote settlement.
