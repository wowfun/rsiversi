# rsi-agent-workspace-context

Agent prose mentions allow a terminating colon followed by whitespace or end of
input, as in `@review: please check`. Quoted file paths, `@path_hex:` locators,
slash paths and colon-connected locators are excluded from Agent mentions.

Explicit skill discovery and body reads use the same precedence and
identity validation as before-step snapshots. Human discovery and preview
require user-invocable; the skill_read Tool requires model-invocable and derives
its Header from AgentCallerAuthority. These independent flags apply equally to
user and project roots. The read operation does not generate invocation Facts.
All filesystem work shares the snapshot owner's admission and lifetime.

This package owns bounded filesystem discovery for model-visible workspace
instructions and skills. Its global snapshot service owns filesystem discovery;
its Agent-only contributor owns direct-user invocation interpretation, digest
comparison, replacement/tombstone inputs and last-good state. The contributor
registers a typed `rsi.workspace-context` domain and returns the actual entered
text with its state proposal. The executor commits the whole stage through the
generic Kernel boundary.

The state carries a Session-and-Turn-bound Fact cursor. A complete snapshot advances the
cursor and digests atomically with its inputs; an incomplete snapshot advances
neither. Each new Turn starts at its exact acceptance, so a claim-filtered scan
cannot skip a previously queued acceptance when its Turn later executes. A fork
likewise starts from the child's acceptance and retains only inherited digests.
History is paged at the captured horizon and only the current Turn's Human
entered content or direct Turn acceptance can
request skills. The filesystem source receives bounded selected invocation
names and never receives a Store writer.

Domain version 2 also records whether an incomplete observation has been reported.
An incomplete observation enters one bounded diagnostic per failure episode,
without publishing any partial baseline, consuming skill requests, or changing
the last-good digests. A subsequent complete observation clears that diagnostic
latch. A project read failure can delay the whole baseline, including user-owned
sources; this is visible even when no complete baseline has yet been entered.
Local observations retain only the first failure's logical path and reason, in
at most 2 KiB including escaped path text. The contributor's diagnostic and
explicit resource read errors include this detail without source contents or
raw operating-system error text. Diagnostic storage shares the snapshot scratch
budget; the incomplete-observation latch and last-good state rules are unchanged.

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
to the finite `from_messages` adapter contain at most 64 messages with 64 content
blocks each. Production scans validated Human Facts incrementally at the captured
horizon. Both paths retain at most 4,096 distinct candidate names before matching
at most 256 selected skills; repeated names do not consume additional capacity.
Capacity and source closure retain their typed contribution error categories.
The first nonempty line accepts `/name` and `/skill name`. Direct Human text also
accepts `$name` anywhere outside code, escaped text and link destinations. Names
use the existing skill-name grammar; unknown names remain ordinary text. References
are deduplicated in source order. The pure reference/token parser is also used by
terminal completion; parsing alone grants no invocation authority.

Cancellation of an async waiter does not release a running job's lane. The
blocking job and any unclaimed result own that lane until dropped. Withdrawal
closes the instance, requests cooperative cancellation between reads and read
chunks, and drains actual jobs. A filesystem call already executing cannot be
forcibly stopped; it continues to occupy process capacity until it returns.

The configured user instruction file, user skill roots and selected Session
workspace participate in discovery by default. Project instructions are ordered
from the nearest Git root to the Session cwd; when that chain exceeds 64 files,
the deepest 64 retain the most
specific policy. Project skills are scanned cwd-to-root from each `.agents/skills` directory,
retaining the deepest 64 directories. Without a Git root, skills use cwd alone.
The nearest project definition wins, followed by configured user roots in their
specified order. Catalog, preview and invocation share this exact selection. Discovery and source reads are
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
This omission also applies to an invalid body discovered beyond the metadata
prefix; it does not discard otherwise valid instructions and catalog context.
An absent optional path is also a complete omission. Any other filesystem I/O
failure marks the observation incomplete, so the contributor preserves the last-good
durable digests instead of publishing replacement or tombstone Facts from a
partial scan.

The render bound is itself a deterministic selection contract, not an I/O
failure. User instruction sections are retained in configured order while they
fit; project sections are selected deepest-first so the most specific policy
wins, then rendered root-to-cwd. The skill catalog retains its lexical prefix.
`complete` means discovery and selected reads formed a coherent observation; it
does not mean every eligible source byte fit the model-visible render. Digests
always cover the exact bounded text proposed for persistence.

Project instruction reads acquire the project root once per snapshot
without following Unix path components and retain it as a directory capability.
Instruction opens stay relative to that handle, including across renames.

Skill roots are discovery locations, not filesystem containment boundaries.
Configured user roots and project roots follow directory symbolic links,
including parent components and targets outside the project. No additional
target allowlist or approval is required. Skill files themselves (`SKILL.md` or
standalone `.md` files) must be regular files, not symbolic links. Discovery
remains limited to the existing immediate entries, with no recursive walk.
Each observation resolves and opens the target directories, retaining their
handles for metadata and body reads. A link retarget or directory rename cannot
redirect an already selected skill; the next observation resolves links again.
Body reads revalidate the selected metadata, and an unavailable or changed
selected source makes the observation incomplete.

Skills retain both their logical discovery path and resolved file identity.
Displayed sources use the discovery path (project-relative for project skills);
resolved identities deduplicate aliases, with the first valid discovery winning.
Name precedence remains unchanged. Entries count toward the scan limit before
deduplication, and retained paths, identities and directory handles share the
snapshot budget. During discovery, missing paths, dangling links, directory-link
loops and file links are complete omissions. A source selected by that observation
which disappears or becomes a file link before its body read instead makes the
observation incomplete; the next discovery can omit it normally. Permission and other unexpected I/O errors make
the observation incomplete. Exact reads distinguish a missing skill from
model-invocation or user-invocation restrictions and from failed/changed reads.
The Unix handle mechanics are shared through
[`rsi-files-native-fs`](../../rsi-files/native-fs/README.md); instruction discovery and
snapshot capacity remain owned here.
The typed Session Header is trusted at this process-local seam; only its cwd
crosses into the blocking discovery task.

The service returns complete instruction and catalog digests. The contributor
recognizes skill invocations only in direct Human content using the syntax
defined above, and places selected skill bodies after its background inputs. Invocation names are extracted from borrowed Fact content
before blocking filesystem discovery; durable message payloads are not cloned.

## Agent definition files

Agent definition discovery shares this source's bounded filesystem ownership.
Project `.agents/agents` roots are visited nearest cwd first through the Git root
(only cwd when no Git root exists); configured user roots follow. Direct `.md` files only are selected by filename,
with complete-file precedence. Directory links authorize their resolved directory;
file links are excluded. YAML requires a nonempty description; the nonempty
Markdown body supplies persona. Optional model/effort and exact Tool allow/deny
are validated at this boundary. Selected malformed files never fall back to a
lower-priority definition. Reads are cancellable, bounded and never cached across
spawn invocations. Listing or previewing a definition grants no execution authority.
Descriptions are workspace-controlled model context, refreshed for each provider
request, and can influence orchestration just like persona bodies or AGENTS.md.
Selecting a workspace accepts these instruction sources; this is not prompt
injection isolation and does not grant Tool, Sandbox or approval privileges.

The standard product supplies `HostPaths.config()/agents` and `~/.agents/agents`
as user roots, in that order. Filenames are role names (for example
`.agents/agents/reviewer.md`). A minimal definition is:

```markdown
---
description: Review changes against the repository contracts
allow: [file_read, directory_list]
---
Inspect the supplied change and report concrete findings with source evidence.
```

`model: { deployment: configured-route, model: model-id }` supplies a default
selection; optional `reasoning_effort` requires that model. `allow: []` exposes
no ordinary Tools, omitted `allow` preserves the parent set, and `deny` removes
exact names. The Turn owner intersects these restrictions with ancestors.
Unknown fields fail definition validation; Tool-name syntax is checked here and
existence is checked against the pinned catalog at spawn admission. Discovery
validity and human readability do not guarantee a spawn's catalog or capacity.
Missing, non-directory and looping Agent roots are omitted; unexpected I/O
errors withhold the catalog and produce escaped, bounded diagnostics.
Discovery admits at most
256 directory entries and 64 KiB per file. Exceeding the directory-entry bound
withholds the entire catalog and exact-name lookup for that observation.
Invalid or non-UTF-8 basenames are
skipped, as they cannot identify an Agent; malformed contents of valid names
remain visible diagnostics. One walk also checks up to 32 caller-reserved inline
names for collisions, including outside the listing prefix. Such collisions
return unavailable entries without reading or parsing the conflicting bodies.
Listings keep the first 32 unique
names in root precedence and filename order; excess names do not invalidate
that prefix. Exact-name reads remain available outside the listing. Description is at
most 1 KiB and persona at most 32 KiB. Selected invalid entries stay visible with
a diagnostic and cannot spawn. Direct Human `@name` mentions exclude escaped
text, code, email addresses, bare URL handles and file locators; they request model
orchestration. A leading `@example.com` is a valid dotted role name, not an email
address with a local part.
