# rsi-dev-control

Application-owned development control messages use child stdin/stdout pipes.
Diagnostics use stderr. The pipe is a local parent/child authority boundary;
the run identifier separates sessions and is not a network credential.

Each UTF-8 JSON line is at most 32 KiB including LF. The closed envelope contains
`version: 1`, a 32-character lowercase hexadecimal `run_id`, and one closed tagged
`message`. Invalid, unknown, duplicate or out-of-order messages close the session.
Owners enforce sequencing and readiness deadlines; this library bounds framing
and validates explicit loopback origins and addresses before use.
This protocol deliberately uses literal `127.0.0.1` and a nonzero bound port;
broader loopback hostname or IPv6 policies belong to the consuming HTTP adapter.

The frontend receives `start_frontend`, reports `frontend_bound` only from its
actual listening socket, receives `configure_proxy`, then reports `proxy_ready`.
The native Web receives `start_backend`, reports `backend_ready`, waits for
`release_launch`, then reports `launch_ready`. Closing the parent's input pipe
retires the child; there are no stop/acknowledgement frames. `open_browser` is a separate bounded CLI invocation, outside the
supervised application processes. No frame containing a launch link is logged.

The parent owns the stage deadlines: frontend and backend preparation each allow
60 seconds after build completion. One 10-second budget covers configuring the
proxy, receiving readiness and releasing the backend. Children retain pipe-EOF
cancellation while waiting; they do not restart competing phase timers.
A frontend listener restart requires a
new development session. The protocol never changes ordinary Web argument grammar.
