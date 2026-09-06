# rsi-agent-workspace-context

This package owns bounded filesystem discovery for model-visible workspace
instructions and skills. It exposes one process-local snapshot service; the
Agent Kernel owns durable Fact insertion and digest comparison.

All instances and generations share four process-wide blocking lanes. Each job
reserves a conservative 16 MiB aggregate envelope, including configuration,
paths, invocation names, selected metadata, source/render scratch and results;
aggregate process admission is therefore 64 MiB. This is a capacity policy, not
a measured optimum. Scratch reservations are conservative and can reject a
snapshot before its returned text alone reaches 16 MiB. Exhaustion returns
`Capacity`, never an incomplete or silently truncated invocation set. Instruction
source buffers retain capacity equal to their admitted byte length; reads
grow with observed data and compact before collection, so many tiny instruction
files do not retain one maximum-size allocation each. Inputs
contain at most 64 messages with 64 content blocks each; invocation extraction
examines all 4,096 possible tokens before matching at most 256 selected skills.

Cancellation of an async waiter does not release a running job's lane. The
blocking job and any unclaimed result own that lane until dropped. Withdrawal
closes the instance, requests cooperative cancellation between reads and read
chunks, and drains actual jobs. A filesystem call already executing cannot be
forcibly stopped; it continues to occupy process capacity until it returns.

The configured user instruction file and user skill roots are trusted inputs.
Project inputs are eligible only when the immutable Session Header says
`trusted`. Project instructions are ordered from the nearest Git root to the
Session cwd; when that chain exceeds 64 files, the deepest 64 retain the most
specific policy. Project skills are scanned only from `<root>/.agents/skills` and
cannot replace an identically named user skill. Discovery and source reads are
bounded: each directory stream stops after the remaining global allowance plus
one overflow probe, and retained entries are sorted before selection. Seeing
the overflow probe makes the observation incomplete instead of scanning an
unbounded directory. Catalog discovery reads at most a 16 KiB metadata prefix
from each selected `SKILL.md`; the opened file's size must already fit the
source limit before its metadata is advertised. The bounded body is read only for an invoked
skill. The invocation read revalidates the selected name, description, and
invocation flags before pairing that body with catalog metadata; concurrent
identity drift makes the observation incomplete instead of publishing a
misattributed invocation. LF and CRLF frontmatter are accepted, and every
rendered result is
bounded by UTF-8 bytes without splitting a scalar value. Malformed, oversized,
or Session-unsafe optional files containing NUL or DEL are skipped.
An absent optional path is also a complete omission. Any other filesystem I/O
failure marks the observation incomplete, so the Kernel preserves the last-good
durable digests instead of publishing replacement or tombstone Facts from a
partial scan.

The render bound is itself a deterministic selection contract, not an I/O
failure. User instruction sections are retained in configured order while they
fit; project sections are selected deepest-first so the most specific policy
wins, then rendered root-to-cwd. The skill catalog retains its lexical prefix.
`complete` means discovery and selected reads formed a coherent observation; it
does not mean every eligible source byte fit the model-visible render. Digests
always cover the exact bounded text returned to the Kernel.

The trusted project authority root is acquired once per snapshot without
following Unix path components and retained as a directory capability. Project
directory enumeration, metadata, and source opens all stay relative to that
owned handle, so concurrent renames or intermediate symlink replacement cannot
redirect a read outside the selected project. A project skill root or entry
that is itself a symbolic link is a complete omission rather than an unexpected
I/O failure. The source is still reopened on each observation, so edits remain
visible before the next provider request.
The typed Session Header is trusted at this process-local seam; only its cwd and
workspace-trust value cross into the blocking discovery task.

The service returns complete instruction and catalog digests. It recognizes a
skill invocation only in direct Human content whose first nonempty-line token is
`/<name>`, and returns the selected skill body separately so the Kernel can put
it last in the Step context. Callers lend message references; invocation names
are extracted before blocking filesystem discovery, so large durable message
payloads are not cloned merely to cross the blocking-task boundary.
