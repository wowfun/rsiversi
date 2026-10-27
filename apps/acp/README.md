# rsi-acp-application

The native ApplicationFactory owns ACP stdio, signals and application task cleanup.
It consumes an ACP attachment supplied by the product connection plugin.
`rsi acp` selects this plugin; there is no separate ACP executable.

Stdio uses nonblocking duplicated descriptors so cancelling an idle pipe read
does not leave a blocking Tokio stdin worker behind. Redirected regular files
use owned file I/O; retirement drains outstanding file work before releasing
descriptors. Stdout carries only NDJSON; bounded categorical diagnostics use
stderr. The Service backend is supplied through a Local contract, and
Application retirement joins protocol cleanup.

Windows duplicates non-inheritable stdio handles without expanding inherited
access rights or changing pipe modes. Pipe I/O uses individually owned native
workers with `CancelSynchronousIo` and thread joins. A read completed before
cancellation retains its bytes for the next reader; cancellation during native
`ReadFile` can discard partial input. Cancelled writes close the transport
because an accepted frame prefix cannot be replayed. Close cancels and joins
every actual worker, including operations whose caller disappeared. No
uninterruptible Tokio global stdin worker is created.
Redirected disk handles use owned file I/O and drain outstanding work on close.
Console/device stdio is refused; ACP launches use pipes or regular files. Ctrl-C
and Ctrl-Break retire the same protocol owner. The product supplies a private
resident Service on Windows and Linux. Each stdio attachment proxies one
authenticated local stream after checking the Host epoch and ACP wire descriptor.
Its native backend lives inside the shared Host, with its own attached-session
set and cleanup. Closing stdio closes that attachment and leaves the Host alive.
