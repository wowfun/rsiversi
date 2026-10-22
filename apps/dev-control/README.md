# rsi-dev-control

Application-owned development control messages use child stdin/stdout pipes.
Diagnostics use stderr. The pipe is a local parent/child authority boundary;
the run identifier separates sessions and is not a network credential.

Each UTF-8 JSON line is at most 32 KiB including LF. The closed envelope contains
`version: 1`, the generated `interface` descriptor, a 32-character lowercase
hexadecimal `run_id`, and one closed tagged
`message`. Invalid, unknown, duplicate or out-of-order messages close the session.
Both peers compare the interface name, revision and complete schema digest before
accepting a message. This prevents a source frontend from silently using a stale
framing/admission contract with a different installed core.
Owners enforce sequencing and readiness deadlines; this library bounds framing
and validates explicit loopback origins and addresses before use.
This protocol deliberately uses literal `127.0.0.1` and a nonzero bound port;
broader loopback hostname or IPv6 policies belong to the consuming HTTP adapter.

The frontend receives `start_frontend`, reports `frontend_bound` only from its
actual listening socket, receives `configure_proxy`, then reports `proxy_ready`.
The ordinary Web or Desktop Application plugin owns the child and the Host
listener lease. Closing its input pipe retires the child; there are no
stop/acknowledgement frames or private CLI development entrypoints.

The parent owns the stage deadlines: frontend preparation allows
60 seconds after build completion. One 10-second budget covers configuring the
proxy and receiving readiness. Children retain pipe-EOF
cancellation while waiting; they do not restart competing phase timers.
A frontend listener restart requires a
new development session. The protocol never changes ordinary Web argument grammar.
