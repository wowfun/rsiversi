# Presentation design

Workspace labels share one path rule across navigation, conversation titles and
composer choices: prefer a supplied display name, otherwise use the final path
component after removing trailing separators; preserve roots and render
`default-workspace` as `Default workspace`.

After an uncertain approval reply, choices stay disabled until an explicit
request refresh reads current pending state. Refresh never repeats the decision.
Questions and plan reviews use the same recovery control and freeze all answer
fields while sending. Reference cleanup retains its original target binding and
can retry across pane changes or a reconnect to the same endpoint and principal.
A failed pending-state read restores the choices' previous state: enabled choices
remain usable, while an uncertain decision still requires successful readback.
An already-settled request closes its panel without reporting a refresh failure.
Menu activation closes its disclosure before awaiting the action. Select changes
that close a disclosure return keyboard focus to its summary.
Replacing content in an open Settings dialog moves focus into the new content;
loading replacements keep focus on the registered host, then return it to an
enabled control when the content is ready.
Closing it restores the original trigger. Pending reference cleanup consumes its
capture identity independently of content deduplication in the draft. Failed
release retains an explicit retry action, including after its preview closes.
An open reference picker retains that capture identity until release succeeds.
Further capture attempts retry its cleanup first; closing joins an in-flight
release rather than issuing a second release for the same identity.
Draft removal, history selection and conversation previews share that cleanup owner.
Composer journal schema 5 carries those capture identities; unsupported saved
schemas are rejected without rewriting their data. When storage cannot open, an
explicit two-step discard action deletes this site's entire saved-draft journal
(including saved request identities) and reloads; it never runs automatically.

The shared Web/Desktop shell displays the [product name](../../../crates/rsi/README.md)
with the canonical [logo](../public/logo.svg). Its full viewBox, grayscale gradient
and transparent background are preserved in every theme, without a background
plate or color filter. Expanded navigation and login show the logo and name;
the collapsed navigation rail uses the logo as its single, labelled expand button.
Hovering that button or focusing it by keyboard replaces the logo with the panel
icon in the same hit area, with an expand tooltip. Clicking the resting logo also
expands navigation, including on touch screens; compact widths open the drawer.
The same SVG supplies the favicon and the source for native application icons.
Vite serves it from `public/` and copies it unchanged into the flat production
bundle as a fixed document-stage bootstrap asset.

DSH tokens retain their fill and indicator roles. The document theme owns separate
warning, success and accent text aliases, with at least 4.5:1 contrast against
their base and tinted surfaces in both themes. Menu material uses the pinned
DSH backdrop filter; vendor files remain byte-tracked by their provenance record.

The composer draft surface has no inner focus rectangle in ordinary themes;
its caret uses the DSH business accent inside the existing rounded container.
Forced-colors mode retains a system-color focus outline. Panel resize hit areas
stay transparent on hover and pointer drag, leaving the column's resting hairline
visible. Keyboard focus marks only the boundary with a two-pixel indicator;
arrow-key resizing and ordinary control focus rings remain available.

Human approval and question dialogs hold one in-flight decision and show failures
once inside the dialog. In-flight delivery exposes busy state; an uncertain outcome
keeps the decision disabled after delivery settles. Known application admission
refusals allow explicit retry unless their owner marks the connection closed.
The Worker and document require that admission marker before advertising retry;
they do not infer it from error messages. An explicit Settings open clears a local
dismissal fence, including after close delivery failed. That fence applies only
to Settings catalogs/editors; other details and resource panels render independently.

The directory bridge preserves typed domain failures. A known validation or
pre-mutation filesystem rejection leaves folder creation editable. An unknown
transport or mutation outcome requires explicit parent readback before retry.
Confirmed creation refreshes the parent before showing the new child column.

Needs attention renders the shared Rust workbench's bounded activity projection.
It displays unknown ownership and truncation explicitly; request buttons send exact
targets back to Rust. The document does not infer execution from recent history,
persist running flags or maintain its own observation poller.

External conversation panes render the Rust controller's distinct identity,
observations and exact permission options. They have no native Goal, preset or
model controls. Close peer is explicit; switching panes only detaches observation.
Drafts follow the shared [retention and revision policy](../../external-drafts.md),
including Web's multiple bindings and pending-text accounting. Their save-status
line retains its height while empty so saving cannot move composer pointer targets.
A pane without a retained draft disables composition;
it retries binding when draft availability changes, without rebuilding history.
Capacity notices never hide an unknown send outcome.
After a rejected edit restores admitted text, both Send and Enter can submit
that text if no other send fence applies. Reset clears admission notices and
disables composition until a new record is bound.

The first-party workbench uses one React 18.3.1 runtime for its workspace/session
navigation, selected detail-mode surface, composer and Session resource column.
The selected DSH SlotCore, React bindings and Button/StateDot primitives are
vendored with immutable provenance and MIT attribution in `vendor/dsh`. Their
Cordis service wrapper and store engine are excluded; RSI supplies bounded frame
sources and exact Session scope identities. Root slots own layout, and scoped
slots cannot render resources from another selected Session.

Composer actions wrap within the available width so every action, including Send,
remains reachable on narrow screens. The composer retains its complete input and
action rows when expanded extension state or attachments consume vertical space.
Its border does not become a clipping
viewport through flex shrink. Narrow workbenches scroll when their content needs
more height; transcript and extension content retain their own scrolling regions.
An unchanged composer action label retains its text node through draft-save
callbacks. Replacing the pointer target between native mouse down and up can
cancel WebKit click activation even when the button itself remains enabled.
Ordinary draft autosaves are silent and do not insert a status row or move
composer controls. Save failures, conflicts and pending request recovery retain
their visible messages and actions.
The docked input starts at one text line, with a taller empty-conversation input.
Keyboard help is the input's accessible description. The ordinary Send action uses a labeled arrow; alternate delivery
and recovery actions retain their visible labels. Session history, commands and
extension state share a disclosure beside the transcript tabs.

Registered Settings use schema-directed controls for bounded simple objects and
scalar/enum fields. Optional fields have explicit inclusion, and unsupported
schema branches use JSON without discarding unknown properties. Switching to JSON
preserves current form edits. The form rejects edited numbers, including nested
JSON fields, whose exact JSON
representation cannot survive JavaScript parsing. Use the full JSON editor for
those values; switching preserves the original numeric tokens from current form
edits, and the full editor forwards the original text to Rust.
The Settings owner still validates values and the exact displayed version
controls replacement.
Apply timing, validation failures and read-only state stay visible; credential
values use the separate setup operation. Plugin diagnostics use the shared
workbench's granted finite read and retain no Local Inspector authority.

Settings strings containing carriage returns use a JSON field, preserving CRLF
and lone CR through edits. Completion options retain their DOM identity across
unrelated frames; request, selection and diagnostic changes update the popup.

File-picker buttons use the same product action handler as the composer, so
synchronous and asynchronous failures reach the visible notice.

Code in a running Turn stays plain while its source grows. Newly settled source
commits as exact plain text first; existing tokens for that exact source may be
reused. One document-owned queue schedules optional
highlighting outside React render, at most one attempt after each animation frame
with a shared 16 ms cooperative tokenization budget. Time-exhausted work may
continue on later frames; retries stop after 500 ms accumulated execution or 32
attempts per block, including lazy grammar initialization. Other incomplete work
stays plain. These cooperative limits cannot interrupt one synchronous operation.
The queue retains
at most 128 jobs and 1 Mi UTF-16 code units of source. Saturation leaves code plain;
replacement, streaming and unmount cancel pending work and retire old results.
Copy and wrapping never depend on highlighting or delay the frame ACK.

Session tools expose the full working directory and Session ID on demand in
both native and external conversations. The shell header keeps the conversation title and controls; the pane header
provides transcript tabs. Coordinates remain selectable in the tools menu.
Idle status is not repeated beside the tabs. A blank surface reloads its starting
draft after reconnect, even when the endpoint and principal have not changed.
The first-party icon sprite includes only glyphs used by the admitted component
graph. Upstream artwork remains unchanged, with a coverage test for new imports.
Settings and resource detail components use strict TypeScript projections of
their Rust-owned data. Runtime field and draft factories expose their consumed
types alongside the implementation.

Successful, exactly paired external delegation Tool cards remain visible in every
detail mode as compact navigation entries, including when their Turn process is
folded. Their navigation button is separate from source inspection. Verbose retains the full Tool text; opening
the card attaches the existing Host conversation and never submits another prompt.

Settings → Plugins also presents reviewed Host leaf management through the shared
Rust workbench. Configuration input crosses the bridge as text so exact JSON
numbers are parsed in Rust. The document retains only an ephemeral input draft;
source selection, grants, preparation, commits and receipt reconciliation stay
with their Rust owners. Saved source, directory durability and current runtime
application have distinct labels. Unknown writes expose the original receipt
query and do not offer a repeated save for that ticket.

External panes keep their switch guard through same-generation frames; only a
confirmed new binding clears it. Both buttons and keyboard submission respect it.

Typing `@name` offers Agent definitions with a human preview. Opening the completion
popup refreshes its catalog once; further keystrokes filter that snapshot. The explicit
file-path button opens the separate workspace picker. Both insert text only;
spawn resolution and definition refresh stay with the Agent contribution.

An unchanged retained transcript keeps message nodes in place, including when its
omitted-history notice is present. Refreshes must not detach action controls
between pointer down and click. Unchanged transcript frames preserve the scroll
position even near the bottom; automatic tail scrolling follows changed content
or a newly bound conversation, so unrelated frames cannot move a pressed action.

Closed human reviews display the exact request and explicit action choices.
Terminal clients accept a choice number followed by optional feedback; Web and
Desktop send the selected stable action and request binding. Free text cannot
approve a review. An answer receipt confirms delivery, not durable approval.

The [product design system](../../../crates/rsi/docs/design-system.md) owns shared
principles. This document owns the GUI expression. System fonts include CJK
fallbacks; semantic CSS variables own light/dark palettes, geometry and states.
External CSS follows system appearance before authentication without inline
bootstrap scripts or a weaker CSP. Authenticated Rust preferences select system,
light or dark appearance and conversation font size. Refresh errors remain visible.

Navigation and resources can collapse and resize with pointer or keyboard controls.
Below 1024px navigation opens as a modal drawer without changing the saved desktop
expanded/rail preference. Its trigger reports the drawer's open state. At wider
widths the same control toggles the saved desktop preference. The command palette uses
Ctrl/Meta+K. Dialogs restore focus to their surviving trigger or composer. Standard
modal triggers are captured before their children mount; initial focus is applied
after `showModal()`, so hidden-dialog `autoFocus` behavior does not choose it.
The command palette starts in its search input, and the directory picker starts
in the path input when manual path entry is shown. Standard
shows foldable Tool and Thinking summaries; Verbose exposes their details.
Process rows use a 24 px disclosure header with a leading icon, tool/Think label
and a single-line preview. Tool names, argument previews and failure badges come
from the typed Tool state; raw arguments and output remain in the expanded body.
The label retains its width when the preview grows; only the preview gives up
space. Unusually long tool names are bounded to 45% of the row.
Standard running Turns keep individual process bodies collapsed until opened and show a
live status at the end of their process, without an extra heading above it.
Per-call model metadata stays in Trajectory for running and successfully completed
Turns; failed or incomplete settled history still exposes it for investigation.
Process disclosures and Turn summaries remain transparent on pointer hover.
Turn expansion reveals process rows; each row owns its disclosure independently.
Failed and incomplete settled history remains expanded for investigation.
Expanding a summary retains its screen position and pauses automatic following.
Stop sends the displayed Turn identity with its attachment generation, preserving
queued input and the draft. It cannot cancel a newer Turn or a pending submission.

Composer keys and the one-time legacy-key migration follow the
[client preferences contract](../../../crates/rsi/client-preferences/README.md).
The sending-options menu exposes the displayed alternate delivery to touch and
keyboard users. Gesture handling captures the action revision before draft storage
flush; a later frame cannot reinterpret that action. Worker and Desktop confirm
successful frame presentation to GUI Rust before it admits that revision.

Workspace ancestry treats backslashes in absolute POSIX paths as literal filename
characters; Windows drive and UNC paths use their own separators.

## Conversation layout

Transcript width is centered and the composer is 32 px wider, limited by the
available viewport. Empty and docked states retain the same composer node in
the same parent; CSS positions it. Process folding, history and streaming retain
a visible block anchor when reading away from the end. A bottom action resumes
following. Tool/source inspection remains available in all detail modes.

Turn presentation writes block visibility and classification classes only when
their values change. Tail-following frames do not read individual message geometry. Reading anchors
are captured only away from the end and batch geometry reads before restoration;
unchanged patches retain block DOM state. Layout record admission measures each
existing record once and trims the least-recently-used prefix under the same
per-record and aggregate byte limits.

Existing installations with the legacy `enter_submit` field migrate to Enter to
send, including the old false default. Other preferences are retained. Choose
Enter to insert a newline again in Settings (`web.submit_key = mod_enter`); busy
submission defaults to Queue and exposes Steer in the sending menu.

Navigation restoration tracks only in-flight group loads (at most 16), releasing
its guard on success or failure. Automatic attempts are triggered by the desired
group set or a new navigation ticket, not by unrelated redraws; explicit expansion
can always retry. Row titles and native attention are indexed once per projection,
with one date formatter and shortest unique same-time ID prefixes.

Protected panes hide queue mutation controls and close the queue editor.
Queue rows reuse the last received array while its pane generation is unchanged.
Unrelated transcript patches do not serialize the queue for a comparison or
replace its controls. Layout persistence failures are visible; the current
connection can still use in-memory layout settings.
Layout reads and save replies cannot replace a newer local layout intent.
Notifications arriving during a refresh request another read, so a coalesced
invalidation cannot leave the document on an older saved layout.

Connection and preference alerts occupy their own flow rows. While an alert is
visible, the connected toolbar also reserves a row so it cannot overlap alert
text or block its controls, including at narrow viewport widths.

Navigation view and Session/workspace order are device preferences. Manual moves
use complete membership before reading 64 ordered summaries; first use initializes
from the full seed's activity order. Session moves stay within exact execution
coordinates and pin/archive partitions. Workspace moves stay within the same
execution location and immediate path parent. An incomplete workspace catalog
cannot prune or reorder saved membership. Keyboard Alt+Up/Down/Home/End and explicit
first/last controls share the same semantic move as drag/drop. A too-large Session
seed preserves saved order and displays updated results with a visible pause notice.

The directory dialog selects Local or one caller-visible SSH target. Changing
location discards the old browsing window; every status, list, create and workspace
registration carries the selected location. SSH availability comes from current
Use admission, independently of Local configuration access. Folder creation is
single-shot, and an uncertain result requires explicit parent readback.
Selecting Open registers the workspace and refreshes navigation without creating
a Session or requiring a model. Session creation is a separate workspace action.
Local Windows breadcrumbs retain their drive root; they never use Unix `/`.

Navigation exposes New session and a Workspaces heading with search, view options
and add-directory controls. The options popover uses grouped, checked rows for
workspace/workspace-tree/single-list grouping, manual/recent ordering, and hide
archived/all/archived-only filtering. All includes active and archived conversations.
Workspace rows use folder icons and reveal their new conversation action on hover
or focus. Each group's local Show more/Show less control exposes its expanded
state; one remaining session uses a singular label. Empty groups need no
explanatory text. External-agent navigation is
temporarily absent from the Web sidebar; its Service contracts and existing
conversation controllers remain independent of this entry. The footer keeps Settings visible and places
Licenses and Deployment checks in its options menu. Conversation options retain
detail level, Compare, Terminal, Restore drafts and automatic width. Global
commands remain available through Ctrl/Cmd+K and the header. Disconnect (or native
Close application) is available from application options and the command palette;
it ends the Service attachment, not an account login. Connected and idle Ready
labels are not displayed. A wide layout uses the sidebar control alone; the header
opens navigation only when the narrow layout has no rail. Missing model setup is
an actionable composer control rather than a global banner. Status and admission errors retain visible
text even where secondary navigation labels are omitted.

All vendored DSH source, including resource dockkit, slots, primitives and platform
tokens, uses `22ddee3792402c3e70ec5f86a63c3e3e895a2f60`. Per-file provenance records
source and adapted SHA-256. Only source is vendored; RSI resolves its own dependencies.
The icon sprite is built from the pinned, static product SVG artwork and mounted
once with the React document root. Runtime content cannot supply SVG markup.
Brand artwork, model marketing labels, fonts and DSH's inline theme bootstrap are
excluded. Platform tokens are external CSS with RSI's appearance selectors.
Renderer classes and legacy color variables remain a global styling contract.
First-party shell, navigation, conversation, Session, Settings and resource rules
live in their owning CSS Modules. Their scope guards add no selector specificity;
semantic classes remain available to fixtures and renderer hosts. Shared element
defaults and the renderer contract stay in the global document stylesheet.
`theme.css` maps those legacy renderer variables to the pinned platform tokens;
components must not redefine platform tokens in terms of legacy aliases.
RSI owns the layout adapter, resource authorities, persistence and Session
selection. The adapter admits two horizontal panes, a 20 to 80 percent split,
16 tabs and four floats per Session. Closing or moving the final tab out of a
pane merges the empty pane as part of the same undoable intent. Explicitly
splitting may leave an empty destination until the next move. At narrow widths,
a single displayed strip combines docked tabs without changing the saved tree.
Float rectangles are clamped to the current viewport for display; resizing the
window never persists these temporary bounds.
Tab-strip scroll positions survive tab selection while their pane exists.
Removed panes release their saved offsets; undoing a pane removal starts its
strip afresh and reveals the selected tab.
Global connection controls align with the conversation column, so dock tab
controls cannot cover disconnect or application close.
Settings has a fixed 188px navigation and independently scrolling options; its
800px shell uses the pinned panel radius and elevation, and becomes fullscreen
with section controls above the options below 768px.
The workspace sidebar exposes External agents when the Host has configured
endpoints or retained external conversations. It supports starting, reopening,
refreshing and paging those conversations. A failed start retains its conversation
identity for an explicit retry; empty Hosts have no external-agent disclosure.
The registered editor fills the options column without a native fieldset border
or inset; its JSON editor retains the same available width.
General, Models, Plugins, and Advanced share this shell. General contains
Appearance and Conversation defaults, including the existing default preset
identity field. Models contains credentials and deployment configuration.
The internal Appearance settings section still routes to its registered owner;
changing the navigation label does not change settings admission or tickets.
Registered settings retain exact local drafts for Appearance and the selected Advanced
namespace until the Settings window closes. Returning to a section reads its
current version; an unchanged version rebinds the draft to the new ticket. A
changed version retains the draft but disables Save until explicit discard and
reload. Choosing another Advanced namespace replaces that section's draft.
A Settings frame renders only in its selected Appearance or Advanced section;
one document owner dispatches section reads, including namespace links from Plugins.
Opening a section fences its previous frame until the new read revision arrives;
loading and failed reads share the section host and expose a read-only retry.
Selecting a namespace from the catalog retains that window revision and accepts
its loading and editor frames; only an explicit section read opens a new window.
Late frames cannot replace a different section. Switching editor modes preserves the untouched JSON bytes and does not dirty a
draft. Save requires valid, bounded JSON. After save or explicit discard, the
editor waits visibly for a fresh read ticket; a Reload settings action can request
that read again without replaying a write or unlocking an old ticket.
Switching sections or editor modes never saves. Returning to a form requires lossless numeric values.
The editor enforces its 1 MiB UTF-8 limit on every mode switch and submission.
Form admission and editing share the same byte-limit predicate and message.
The UTF-16 length precheck avoids encoding short values: each code unit needs
at most three UTF-8 bytes, including replacement of unpaired surrogates.
An initially oversized value is read-only in JSON mode; newly oversized edits
remain editable so they can be reduced, but cannot be submitted. Read-only state
also applies to controls rebuilt after switching modes.
Conversation selection remains visible directly in the toolbar at narrow widths.
Transient composer, delivery, conversation, session and source menus use native
focus order, outside dismissal, and the innermost
unmodified Escape; IME composition never dismisses them.
Verbose source actions remain inline and do not participate in disclosure dismissal.
Closing a menu action restores focus to its summary if focus was inside the menu.
Transcript regions are named and keyboard focusable without announcing every streamed text change.
Stable tab hosts retain visited terminal
followers across selection, movement, floating and fullscreen. Closing a tab
detaches; only explicit termination ends its shell. Restoring terminal coordinates
never creates a shell or silently takes its writer grant. The terminal roster
refreshes when its tab becomes active, so retained hidden UI cannot conceal
shells created since the prior observation.
Only a visible, nonzero terminal viewport with finite measured cell counts can
request a resize. Hiding a retained host never resizes its remote shell.

Application extension restoration sends the ownerless application command;
Session extension restoration carries its current pane and generation. Narrow
resource-overlay visibility is transient and never writes the saved desktop
visibility, including when opening a resource tab. Collapsing docked panes on a
narrow viewport preserves the focused floating pane and the root dock selection.

The starting page has no resource column or resource-toggle action. A saved open
column is suppressed until a conversation is selected, without overwriting that
desktop preference. A selected external conversation can show its resource
summary; no Session dock or terminal is allocated for the starting page.

The first-use composer selects the Service-provided default workspace, or an
existing registered workspace when available. Its input remains usable before
model configuration; the model control opens setup when no model is available.
The workspace selector chooses registered workspaces and offers the directory
picker for adding one. Composer controls expose only implemented actions.
Conversation headings and underlined tabs separate the transcript from execution
activity. User messages are compact right-aligned bubbles; assistant output uses
the shared conversation width with its actions beneath the reply.

Workspace rows use the active conversation's workspace to color the folder icon.
Conversation activity uses an accessible leading status mark: a muted animated
ring for running, a waiting mark or unread dot for those observed states. Unknown
is distinct from running; reduced-motion preferences stop the ring animation.
The sidebar footer has one More disclosure containing Settings and application
actions. Its menu supports outside/Escape dismissal and focus return.
Hovering a registered workspace for 800ms, or focusing its title, reveals its
name, full path and recorded creation time in a viewport-bounded card. The row's
actions offer Rename and Delete workspace; Ungrouped has neither action.
Rename is persisted by Workspace and shared by navigation and composer pickers.
Delete requires confirmation, removes only the registration, preserves files and
Session logs, and refreshes those Sessions into Ungrouped. Pending mutations
cannot be submitted twice; failures keep the dialog and entered value available.
Activity uses a compact dot with an accessible label and tooltip.
Conversation selection fills the group width; text remains aligned with the workspace
label. Relative activity times refresh once per minute. Each group initially shows
five loaded conversations, keeping a selected conversation visible, with an explicit
expand/collapse control independent of server pagination.

Message clocks use durable Fact timestamps; transient blocks have no fabricated time.
Completed Turn headings show recorded elapsed time when both boundaries are known.
Assistant action rows expose copy, source inspection and reported usage for the final model response.
Feedback and branching controls require their own implemented capability.
The composer statistics row reads the existing caller-scoped Session metrics API,
separately from paged transcript windows. Token totals, cache ratio and the last
request's input-capacity ratio retain the API's completeness and captured-model
semantics. Missing values stay absent.
The docked composer reserves a single statistics line before readings arrive.
Loading, failure, partial and complete readings do not move the input card.
The centered empty conversation has no statistics space. Narrow statistics stay
on one line with ellipsized labels; their full readings remain accessible in
the buttons and detail panels.
Metrics refresh after durable settlement, never on streamed text deltas; failed
reads and unfinished reductions offer an explicit retry in the statistics details.

The composer footer has separate Session statistics, Token usage and Context usage
buttons. Each opens one nonmodal, viewport-clamped panel above its own button,
with Escape/outside dismissal and keyboard focus restoration. Message usage uses
the same panel skin. Token detail partitions inclusive input by reported cache reads; cache writes
are a supplementary subset of uncached input and reasoning is a subset of output.
Unknown subsets are not presented as zero. Session statistics use whole-log recorded model
time, accepted-to-terminal wall time, mean TTFT and decode throughput. Context
shows the last reported input and captured input capacity; system/tool/message
subdivisions are absent because the current metrics owner does not report them.

Models settings show configured provider cards with one inline editor. API key
is the primary field; blank preserves saved credentials. Customized settings
disclose endpoint, protocol and bounded model definitions. Supported families
come from RSI managed providers. Editors retain their opening revision and
untouched fields across section switches; closing Settings clears transient secrets.
Operation failures remain visible in Models alongside provider diagnostics.
Credential, provider and default model writes have distinct
outcomes. First setup selects the first configured model only when no default
exists. Keys never enter view state. Explicit discovery may use an entered key
without saving it; fetched candidates are not silently enabled routes.
The model editor keeps request output caps within the edited model capacity.
For new DeepSeek models it explicitly uses the default output reserve as the
request cap. Existing caps are retained unless the new maximum is smaller;
per-model settings for removed model IDs are removed with their definition.

Access choices stay disabled until preparation has attached the conversation's
draft editor. Failed or unavailable preparation never enables those choices.
The composer offers the same model/effort, add-input and access menus before and
after the first message. Opening a control that needs Session capabilities opens
one draft Session and transfers the saved starting text without submitting it.
Concurrent controls share that operation; failed or replaced attachments retain
the starting draft. Model choices use the configured catalog and effort choices
use the selected model's declared profile. Selection uses the existing durable
model command and its uncertainty receipt. Menus open above their trigger, stay
inside the viewport, support keyboard navigation, Escape and outside dismissal,
and restore trigger focus after selection.
The starting composer reads the configured default effort and the exact default
model's Host-described profile during setup, before any menu is opened. Missing
or mismatched descriptions never borrow another model's capabilities. Preparing
an empty draft does not introduce a header row or move the composer.

Back to bottom is an accessible chevron icon and is visible only when the
transcript has scrollable content more than 70 px below the current position.
Expanding a turn preserves the reading anchor without itself enabling this
control. Scroll, transcript updates and viewport resize recalculate visibility.

Access selection applies to subsequent queued messages from this browser, never
to an active Turn or a frozen retry. Read Only, Workspace Write and Full access
map to the typed sandbox request modes. Full access requires explicit confirmation
and still retains RSI's mandatory tool approval. Steering keeps the active Turn's
policy. The add menu exposes supported image import, workspace file references,
conversation references, history and the Session's actual command catalog.
Picker lifetimes are distinct from the metrics and other dialog lifetimes;
closing a picker removes its modal and returns focus to the original editor.
