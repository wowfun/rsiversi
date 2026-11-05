---
name: Verbatim file inputs shared with application publishers
---

## Problem

The original DSH uploader streams immutable files to disk. Converting
a file receipt to plain text loses durable file identity. Importing the same
file into a second store adds I/O, disk use and ownership without improving it.

## Decision

Media owns a separate locator-free file reference and a bounded read service.
The local reader consumes the immutable digest/name namespace used by the DSH
publisher. Applications pass file references after resolving their own upload
receipts; native admission verifies the referenced bytes in its configured
namespace. Browser input cannot choose a root or pass a trusted host path.

Native Session and model input retain a typed file block. At provider request
assembly, resolve a readable path in the actual execution environment and emit
an explicit file handle, as DSH's `fileHandleText` does. File bytes stay outside
the prompt until tools read them. Unavailable execution paths must be reported
as unavailable rather than pretending the model has read the file.

The Context fold records whether the receiving Session executes locally, but
does not depend on a file reader. This keeps checkpoints location-free and also
covers compaction requests. The Provider SDK captures the optional reader at
Prepare and formats a handle during serialization without reading bytes. The
Portable provider carries those derived handles alongside its immutable request;
rewriting the retained request would require another allocation and would change
the caller's request identity. The additional metadata avoids dependency RPCs.

Session resolves the optional service when admitting an input because user
Profile plugins can activate after the standard Session service. Capturing an
absent optional reader at startup would incorrectly require a different startup
order. The selected reader is retained across verification of that file.

## Alternatives considered

Using conversation references conflates different capture and read contracts.
Encoding a file handle as submitted text discards identity in history and UI.
Copying uploads through JSON IPC into another CAS creates a second durable
owner and makes large uploads consume whole-file memory. Persisting host paths
would bind content identity to one Home and bypass the configured namespace.

## Consequences

Original upload receipts bind to native mailbox acceptance using DSH's own
commit/dispose guard. The typed file survives request hooks, queue receipt
recovery, native checkpoints and original browser history. A lost submission
reply recovers the same native identity without submitting again. Published
DSH rc.4 and rc.5 exercise the contract without a source revision allowlist.

Native format changes invalidate old Session and context caches rather than
adding migration branches. Byte verification occurs once at external input
admission; provider serialization never reopens or rehashes the file. Large
file references do not consume image budgets or require whole-file buffers.

Windows and Linux tests cover actual published objects and namespace failures.
Linux browser acceptance includes a real DeepSeek call reading a random uploaded
file through Bash and unchanged native Facts after Host restart. Native macOS
is not exercised. The workspace-relative native file tool cannot address this
absolute upload path; the shell can. Non-waking context injection still carries
text, and normalized image receipt import is separate unfinished work. These
are capability gaps, not permanent design trade-offs.

A shared namespace is a storage contract, not a DSH version hash. Same-user
edits can corrupt immutable files; admission detects content changes but the
system does not claim isolation from that user.
