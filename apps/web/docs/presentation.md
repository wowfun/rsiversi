# Presentation design

The Session panel button sits in the selected conversation's title toolbar,
beside the workbench controls. Its panel opens below the Chat tabs and shows
the attached working folder and its parent.
The folder exposes its full path to pointer and keyboard users. Its actions
button, context click and keyboard context menu offer Copy folder path, which
copies the exact attached path and reports the clipboard result. Show in folder
uses the serving Host's file manager when the caller owns that desktop and the
attached folder is local. The first directory menu on a connection checks desktop
availability, and opening later menus reuses the answer. Escape returns
focus to the control that opened the menu and keeps the panel open. With 348 px of
space beside the transcript the panel docks there and follows the browser's
docking preference. In a narrower pane its button opens a temporary overlay.
Escape or a press outside closes it. The panel belongs to the selected Chat;
the header control is unavailable in Trajectory.

A conversation with an active, paused or blocked Goal shows its recorded phase
and objective above the composer. Completed goals, protected interactions and
global workbench pages leave the strip out of the composer. A current Goal
surface lets the user open its controls from the strip. Reading the strip uses
the attached conversation's projection without opening a resource.

A conversation with a nonempty to-do projection shows its progress above the
composer. The panel starts collapsed and names each status with a nonzero
count. Expanding it shows the current items in their supplied order with
accessible status labels. A replacement updates the open panel, and clearing
the list removes it. An interaction that takes over the composer hides the
panel until the composer returns. Opening another conversation starts its
panel collapsed.

The automation task's action menu offers Run now for active, paused and ended
tasks. It disables delivery while the linked Session is archived, while a
delivery is running or until the one-minute interval expires. The manager
and Session task views sort and display the earliest eligible run, including
that interval. An unsuccessful request shows its error beside the task.
The automation task manager confirms deletion against the selected task and
Session. Confirmation removes future triggers and keeps queued messages and
the original conversation. A successful deletion returns focus to the list.
On desktops the task detail occupies 47% of the manager. At viewport widths
up to 760 px the detail takes the full manager width and the list is hidden.

Feedback actions follow Copy on a settled local assistant answer. Hover, focus
or activation reads the rating for that answer. Choosing the active rating
removes it. Choosing the opposite rating opens a fresh optional category and
note. The dialog retains its draft on a save conflict and shows the current
rating. The initiating button keeps focus while its initial read is pending;
closing the dialog returns focus to that button. Pane generation and the
immutable message coordinate bind every request. Feedback has no browser persistence.

Manual compaction occupies one chronological row in the issuing conversation.
Its command identity anchors progress and settlement across observation, paging
and reopen. An installation marker enables disclosure of the cited summary
when its text is present in the loaded window. A busy or empty command shows
its outcome in the issuing pane without publishing an empty session.

Non-human messages that start a Turn appear above its process disclosure. The
trigger header shows its recorded source family and time. Opening it reveals the
original notification text and its source actions. The body starts folded on
attachment and is unmounted when closed. Runtime context keeps its own disclosure.
An external schedule change consumed by a Step is folded into that Turn's
process below its opening input. Expanding the process and its context row
reveals the original text in a scrollable code panel, even when the notice
reached the inbox before the input that started the Turn.
Confirmed task deletion shows a brief success banner that remains visible when
the operator returns from the manager to the conversation.

Expanded and collapsed workspace navigation both offer Plugins followed by
Automation tasks. Plugins opens a workbench page with the first page of the Host
inventory. It reuses a loaded first page from the current Host. Refresh reads
the inventory again. Rows identify configured instances and their observed
state. Selecting a row opens its details. A back arrow returns focus to that
row, and a labeled refresh button reads the inventory. Host configuration and diagnostics
remain accessible from the page through Settings. Selecting the already attached
conversation from a global page reveals its existing binding, draft and reading
position. Returning to its visible transcript starts a new reading visit.
Opening either global page closes the other
and releases any task observation. Opening the task manager subscribes to the current Host catalog
while its view loads. It displays each complete update atomically, including
changes made by another device or an automatic turn in a closed conversation.
Closing the manager releases its observation and results. The initial list shows
active tasks. Each status lists tasks by next run. A row's accessible name is
its title, with status, frequency and next run in its description. Selecting the
row identifies the expanded detail panel. Active rows and their manager detail
show the same local timestamp and relative time, refreshed every 30 seconds.
Search ranks task titles and
matches session IDs and prompt text across query terms, ignoring case, width
and Latin diacritics. Removing the selected task from the catalog closes its
details and returns focus to the heading. Closing the manager returns
focus to its navigation control. Selecting a task shows its details. Escape closes the
details and returns focus to the task row. Clearing a search restores the unfiltered
list and keeps the search field ready for typing. Open session returns to
its conversation. Create in a session appends the proposal to the selected
pane's starting draft and leaves submission to its user. While the catalog is
visible, each pane keeps its frame acknowledgement host and draft controller.
The native transcript releases its rendered messages and restores its reading
position when the conversation returns. Empty enabled and all-task lists offer
the DSH recommendations. Choosing one appends its request to the starting draft.

Closing the saved-draft panel or successfully opening a saved conversation
returns focus to the composer. Generation failures first observed in a background
pane are announced once when that pane is selected, if still outstanding.

A session row's secondary click, Menu key, Shift+F10 and actions button open the
same menu for that row. The active conversation stays selected. Arrow keys move
between enabled actions. Escape returns focus to the opener. Only the open
menu mounts its controls. Activity indicators occupy their own space in flat and grouped rows. Rename opens a labelled dialog for the chosen Session. Workspace and Session dialogs use the opaque secondary background. Confirming an input-method composition keeps the Session name editable.

An expanded workspace shows the selected unpublished Session as **New session**.
Selecting it returns to its composer with its input intact. Its row comes from
the open pane and gives way to the Session's navigation entry at publication.

On the first keyboard submission, the composer keeps its keyboard position
while the starting draft binds to a Session. Its text is read-only during that
handoff. A user who moves to another control keeps focus there.

At a composer width of 460 px or less, the access selector retains its shield,
chevron and full accessible name. When the expanded controls need more than one
line, the model selector first replaces its name and effort with the Models
icon. Its accessible name retains both values. When even the icon cannot fit,
the model and send actions wrap together. The add menu remains anchored above
the composer.

The model list reads and filters the catalog while its submenu is open. The
closed trigger and the root menu use the current selection and effort profile.
A starting composer displays the Agent's configured default reasoning effort.
With no explicit default, it displays the selected provider's default effort.
After publication it follows the Session's own selection.

Each completed native Turn offers a fork into a new draft: full history, a positive
number of recent local Turns, or no history. The action flushes the local editor
and binds the current pane generation before requesting creation. The Session
owner resolves the terminal cut and publishes only on the draft's first input.

An open conversation with enabled automation tasks shows a clock in its header.
One task opens its detail directly in the right dock. Multiple tasks open a list
ordered by next run. Schedule timestamps use the interface language and the
device time zone, with a 24-hour clock. The open list updates the distance to
each deadline once per second. Closing it stops the clock. The detail reads the current Session's Schedule projection,
including its prompt, frequency, next run and last accepted occurrence. The dock
retains the task ID. The prompt and delivery data stay in their owning Session.
Each completed turn that creates a task shows its creation card below the final
reply. Opening the card selects that task in the Session's right dock. The card
uses the persisted result for its initial title and frequency.

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
Workspace choices dismiss on selection, Escape or outside interaction. Fork
history selection keeps its form open for the turn count and Create draft action;
the fork summary remains available in every presentation mode.
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
The composer journal carries those capture identities; unsupported saved
schemas are rejected without rewriting their data. When storage cannot open, an
explicit two-step discard action deletes this site's entire saved-draft journal
(including saved request identities) and reloads; it never runs automatically.
The same action includes pre-Session drafts and unsent question progress for
every endpoint on this origin. Draft writers retire before
deletion; a blocked or failed deletion keeps composition disabled and reports
the incomplete reset instead of reloading. Retired editors cannot reopen storage.
Pre-Session drafts have an origin-wide limit of 128 records and 4 MiB of text,
independent of the Session journal's limits. Empty records without a workspace
selection are removed. Saved-draft recovery includes starting drafts; nonempty
records are never evicted to admit another draft.

Command descriptors explicitly declare composer eligibility and argument input.
Both command menus and slash completion use the same selection operation.
Typing a leading command or restoring one loads the Session catalog, coalescing
keystrokes while that read is pending.
The application's `/export` also exposes its argument hint and Leave command
input action without depending on a Session command descriptor.
No-argument commands preserve the editor and consume only their captured trigger
after a known successful receipt. Argument commands require a leading trigger,
retain the tail as arguments and show the command being edited. Selection and
asynchronous trigger consumption compare the captured editor revision; neither
can overwrite later typing. Structured-only commands retain their dedicated UI.

The shared Web/Desktop shell displays the [product name](../../../crates/rsi/README.md)
with the canonical [logo](../public/logo.svg). Its full viewBox, grayscale gradient
and transparent background are preserved in every theme, without a background
plate or color filter. Expanded navigation and login show the logo and name;
the collapsed navigation rail uses the logo as its single, labelled expand button.
Hovering that button or focusing it by keyboard replaces the logo with the panel
icon in the same hit area, with an expand tooltip. Tapping the resting logo also
expands the rail.
The same SVG supplies the favicon and the source for native application icons.
Vite serves it from `public/` and copies it unchanged into the flat production
bundle as a fixed document-stage bootstrap asset.

DSH tokens retain their fill and indicator roles. The document theme owns separate
warning, success and accent text aliases, with at least 4.5:1 contrast against
their base and tinted surfaces in both themes. Menus and inline image previews
use the DSH menu tint and backdrop filter. Preview panels and attachment paging
buttons use the panel shadow with their own hairline color.
Imported controls use the corresponding DSH font size, weight and line height.
Native dialogs use the theme's modal mask color and filter.

The composer draft surface has no inner focus rectangle in ordinary themes;
its caret uses the DSH business accent inside the existing rounded container.
Keyboard focus in that surface marks the outer composer boundary.
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
persist running flags or maintain its own observation poller. The Session menu
and Ctrl/Command+Shift+U toggle the shared unread reminder on an unarchived
Session. A visible, focused transcript acknowledges a completion when its terminal
record or the start of a later displayed turn has arrived.
An explicit reminder survives repainting the same Session. Opening it again
clears the reminder after display.

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
Keyboard help is the input's accessible description. The ordinary Send action uses
a labeled arrow. While a local submission is in flight, its button shows a
spinner and names the current operation for assistive technology. The busy
button keeps its fill under the pointer, and reduced motion keeps its indicator
still. A saved request
whose outcome needs user action exposes its recovery controls once the submitting
pane is idle. Alternate delivery and recovery actions retain their visible labels.
Session history, commands and
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
highlighting outside React render, at most one slice after each animation frame
with a shared 16 ms cooperative tokenization budget. Each job retains completed
lines and Shiki grammar state; a later slice never repeats that prefix. Only the
requested language is initialized and warmed once; other bundled grammars remain
unloaded. Work stops after 500 ms accumulated
execution, including initialization. An incomplete or over-budget line stays
plain rather than continuing from an invalid grammar state. These cooperative
limits cannot interrupt one synchronous operation.
The queue retains
at most 128 jobs, 1 Mi UTF-16 code units of source and 32,768 retained raw tokens.
Each block admits at most 256 KiB of UTF-8 and UTF-16 source, 4,096 characters per
line and 16,384 rendered tokens. Saturation leaves code plain;
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
source selection, grants, preparation and commits stay with their Rust owners.
Saved source, directory durability and runtime application at completion have
distinct labels. Unknown writes remain unknown; refreshing current source cannot
prove the interrupted operation's result. Results name the affected leaf and
operation. Closing a focused review returns focus to its result or source choice.

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
External transcript changes are compared by block content and conversation binding,
not decoded array identity. Busy and status updates alone preserve the viewport.
Model-search editing keeps native caret and selection keys. Left Arrow returns
from a submenu only on its menu items; initial Up and Down select the last and
first available menu items respectively.

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
Their handles track one primary-button drag, preview at most once per paint and
apply the width at release. A stationary press or cancelled drag keeps the
previous preference. Keyboard increments start at the rendered width.
Below 768px navigation overlays the conversation. Opening the drawer focuses its
collapse control. Opening it for session search focuses the search field instead.
Escape and a press on the backdrop return focus to the opener.
Selecting a conversation or a destination closes the drawer. Reopening it preserves
its search and workspace controls. From 768px through 1023px, the 56px rail
expands in place and the conversation remains interactive. Crossing either
breakpoint resets the temporary expansion. Opening resources at a compact width
also collapses navigation. Each desktop window starts with a 280px navigation
column. Dragging adjusts it between 264px and 420px for that window. A temporary
compact layout preserves the desktop choice. Expanding a collapsed desktop rail
and refreshing the window both restore 280px. Opening search from the rail
uses the same expansion. These gestures perform no device storage writes.
The command palette uses
Ctrl/Meta+K. Dialogs restore focus to their surviving trigger or composer. Standard
modal triggers are captured before their children mount; initial focus is applied
after `showModal()`, so hidden-dialog `autoFocus` behavior does not choose it.
Tab from the last available control returns to the first, and Shift+Tab
from the first returns to the last. An open nested dialog or focused menu
owns its own keyboard navigation. Escape dismisses only the top dialog and
returns focus to its opener inside the parent dialog.
The command palette starts in its search input, and the directory picker starts
in the path input when manual path entry is shown.
Picking a directory focuses its current row in the parent column. New folder
opens a modal for the selected directory. Escape returns to the picker, and
successful creation selects the new child, including a hidden directory.
Breadcrumbs begin at Home when it is an ancestor and at the filesystem root
elsewhere. When a long path opens, the trail scrolls to its selected end.
Each segment is a navigation button. The remaining space and pencil open the path editor.
Editing a newer path supersedes a submitted listing. Escape in the path editor
restores the last listed location and returns focus to the path control.
Leaving a location or closing the picker cancels its outstanding directory read.

The four transcript detail modes use this presentation policy:

| Mode | Running process | Completed process | Preview |
| --- | --- | --- | --- |
| Compact | Folded with individual disclosures | Folded | No live or settled reasoning preview |
| Standard | Individual bodies collapsed until opened | Folded | Live status and settled reasoning previews |
| Detailed | Bodies expanded | Folded, with individual disclosures when expanded | Live status and settled reasoning previews |
| Verbose | Bodies expanded | Bodies expanded | Settled reasoning preview; no duplicate live preview |

Native conversation tabs and narrow-screen options always have a nonempty title:
the saved conversation title, workspace name, or “New conversation”. Unrelated
view publication does not rerender the conversation container when its surface
keys, selected surface and titles are unchanged. Command-input announcements
contain only the command label and hint; the Leave command input action is outside
that live region.

Assistant replies remain visible in every mode. Failed or incomplete history
stays expanded for investigation. These are RSI process blocks; the interface
does not claim DSH Agent-Step grouping that the Turn index does not provide.
Assistant copy, fork, usage and time appear at the settled Turn boundary.
Copy includes the displayed prose of the last assistant response containing text,
including narration before an interruption. During execution the user's copy
action and source inspection remain available. Message and code copy controls
show a check for one second after a successful write. Repeated activation while
the write or its confirmation is pending leaves the clipboard untouched.
Process rows use a 24 px disclosure header with a leading icon, tool/Think label
and a single-line preview. Tool names, argument previews and failure badges come
from the typed Tool state; raw arguments and output remain in the expanded body.
Closed process rows retain their disclosure and exact source binding. Their
full text, Markdown and inspection controls mount when the body is opened.
While reasoning streams, the collapsed Think row previews the first complete
line of the latest paragraph. At settlement it previews the first line when
the selected detail mode shows reasoning previews. Opening the body removes
the duplicated text from the header.
Settled Tool argument previews remain available in every detail mode.
The label retains its width when the preview grows; only the preview gives up
space. Unusually long tool names are bounded to 45% of the row.
Live status appears at the end of the process, without an extra heading above it.
Chat mounts per-call model metadata when the detail mode or turn outcome exposes
it. Trajectory can inspect these records even while the corresponding Chat row is
absent. Failed or incomplete settled history exposes its diagnostic metadata.
Trajectory groups loaded records by turn, preserves the chronological position of
between-turn activity, and filters the ledger by all terms in a case-insensitive
search. The DSH timeline places input, model, and tool activity on separate
lanes. Dragging a range filters the ledger, clicking a span selects its event,
and double-clicking clears the range. Recorded-time mode uses only observed
timestamps and leaves missing timings unknown. The details
show the projected record and expose the existing source selectors on demand.
Search, selection and fold state belong to the current conversation view.
Chat's column resize handles and return-to-latest control apply only to Chat.
A completed turn's fork action sits beside Copy and usage on its final assistant
message. A turn ending without a visible assistant reply keeps the action at
its terminal boundary. The action exposes the full, recent, and empty history choices.
Process disclosures and Turn summaries remain transparent on pointer hover.
Turn expansion reveals process rows; each row owns its disclosure independently.
Summary counts include only Tool and reasoning rows. A Turn with no such rows
shows its status without a disclosure control.
Expanding a summary retains its screen position and pauses automatic following.
Stop sends the displayed Turn identity with its attachment generation, preserving
queued input and the draft. It cannot cancel a newer Turn or a pending submission.

Composer keys and the one-time legacy-key migration follow the
[client preferences contract](../../../crates/rsi/client-preferences/README.md).
Leaving command input follows textarea editability and does not mutate text
during composition. Restored slash commands fetch their catalog using the same
name grammar as command claims, including digit, underscore, dot and hyphen starts.
Choosing an argument-taking command from the menu preserves the entire current
draft as its arguments, regardless of selection. Completion replaces only its
leading trigger; it refuses a trigger following non-whitespace prose.
Selecting the already entered command keeps its existing prefix and arguments.
Selecting a Skill in slash completion places the caret after the separator,
ready for its arguments. Existing arguments and whitespace remain in the draft.
A menu capture is consumed by one selection attempt; later explicit selections
capture the current editor again, while stale attempts still fail before editing.
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
End resumes following only when the transcript region itself owns the key event.
Handled events, composition, and Shift selection remain with the focused editor.

Turn presentation writes block visibility and classification classes only when
their values change. Tail-following frames do not read individual message geometry.
Activating a turn heading anchors that heading in place while its process rows
open or close. For other changes while reading history, the transcript locates the first visible row in display order.
It keeps the visible rows and one subsequent row as fallbacks if folding removes the first anchor;
unchanged patches retain block DOM state. Layout record admission measures each
existing record once and trims the least-recently-used prefix under the same
per-record and aggregate byte limits.

Existing installations with the legacy `enter_submit` field migrate to Enter to
send, including the old false default. Other preferences are retained. Choose
Enter to insert a newline again in Settings (`web.submit_key = mod_enter`); busy
submission defaults to Queue and exposes Steer in the sending menu.
Both the conversation and external composer accept a single Ctrl or Command
modifier for modified Enter. Shift+Enter inserts a line break. Alt, AltGraph,
Ctrl+Command, and Shift with Ctrl or Command leave the draft and an open
completion list in place. Holding a submit key down sends once.

Navigation restoration tracks only in-flight group loads (at most 16), releasing
its guard on success or failure. Automatic attempts are triggered by the desired
group set or a new navigation ticket, not by unrelated redraws; explicit expansion
can always retry. Row titles and native attention are indexed once per projection,
with one date formatter and shortest unique same-time ID prefixes.

Claimed inputs leave the queue as soon as the pane receives the updated queue,
including while its to-do list is open. Protected panes hide queue mutation controls and close the queue editor.
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
registration carries the selected location. The picker reads remote targets when
the connection offers the SSH catalog. It shows the location selector when an
SSH target grants Use or when the user needs to leave a selected remote target.
Local browsing follows its own configuration access. Folder creation is
single-shot, and an uncertain result requires explicit parent readback.
Selecting Open registers the workspace and refreshes navigation without creating
a Session or requiring a model. Session creation is a separate workspace action.
Local Windows breadcrumbs retain their drive root; they never use Unix `/`.

Navigation exposes New session and a Workspaces heading with search, view options
and add-directory controls. The options popover uses grouped, checked rows for
workspace/workspace-tree/single-list grouping, manual/recent ordering, and hide
archived/all/archived-only filtering. All includes active and archived conversations.
Session search focuses its input on open. The rail search button and
Ctrl/Command+Alt+K open navigation and focus that same field. Pressing the
shortcut again returns focus to the existing query. It ranks visible titles and workspace
labels by their text match, also accepting session ID and path substrings.
Equally ranked rows show the most recently active conversation first. The
selected archive filter applies throughout. A complete, current first
page and its pins answer metadata searches locally. Otherwise, after 250 ms
without an edit it searches the navigation service; Enter dispatches immediately.
Expanded groups restore when the workspace list returns. Results and continuation
controls belong to the displayed query.
Escape clears and closes search. Clicking outside an empty search closes it.
Search rows show the workspace below the session title so identically named
conversations can be distinguished before opening them. Selecting an active
result closes search and reveals the session in its workspace. Archived search
results describe how to reopen them and offer an Unarchive button on the row.
Selecting one shows a temporary warning above the workspace and keeps the query.
Copy Session ID reports success or failure in the same temporary notification.
Archiving the selected idle conversation returns that pane to its starting
composer once the Host accepts the change. Switching to a different conversation
while the archive is pending keeps the new selection. Reopening the archived
conversation in its original pane restores its saved draft.
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

The [vendor provenance record](../vendor/dsh/provenance.json) pins each DSH
source revision and its source and adapted SHA-256. RSI resolves its own dependencies.
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
General settings offer Light, Dark and System in that order. The pressed option
identifies the selected preference independently of the system's current color.
Choosing a different option in a clean form saves it immediately. If other client
preferences are already being edited, the selection joins those edits for explicit
Save settings. A clean selection previews its colors in the current document
while its versioned write runs. Completion restores the Host projection,
including the previous colors when a write fails.
The browser chrome follows the document canvas using one theme-color meta element.
System appearance follows OS color changes. A fixed preference survives an OS change.
RSI owns the layout adapter, resource authorities, persistence and Session
selection. The adapter admits two horizontal panes, a 20 to 80 percent split,
16 tabs and four floats per Session. Closing or moving the final tab out of a
pane merges the empty pane as part of the same undoable intent. Explicitly
splitting may leave an empty destination until the next move. At narrow widths,
a single displayed strip combines docked tabs without changing the saved tree.
Float rectangles are clamped to the current viewport for display; resizing the
window never persists these temporary bounds.
Inner split separators support axis arrow keys, Shift for larger steps, and
Home/End for their bounds. Floating panels expose focused move and resize grips:
arrow keys adjust either axis, Shift increases the step, and Home/End select
the position or size bounds. These controls publish the same undoable intents
as pointer gestures and retain focus after each adjustment.
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
Appearance and Conversation defaults, including the installed preset selector.
Models contains credentials and deployment configuration.
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
Its own reload retains the mounted editor and keyboard focus while its controls
are disabled. Read failures remain in that draft's recovery surface.
Reads initiated outside that editor replace its view even for the same namespace.
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
Completion and attention announcements track turn and pending-request identities.
Initial history, reconnects and completed background activity are not replayed on selection;
new attention requests received in the background announce when selected if still pending.
unchanged turn and pending projections require no repeated history scan.
Each surface remembers the latest 4,096 event identities for its current Session,
including identities that leave the visible window. Switching Sessions resets
this bounded memory; an older evicted identity can be announced if it reappears.
Rejected plugin replacement appears beside the conversation as a notice that the
previous plugins remain in use, independently of the collapsed extension views.
Its current failure message also uses the always-mounted polite live region,
including when first opening the selected pane. Draft save
failures have their own mounted status region beside the retained input.
Stable tab hosts retain visited terminal
followers across selection, movement, floating and fullscreen. Closing a tab
detaches; only explicit termination ends its shell. Restoring terminal coordinates
never creates a shell or silently takes its writer grant. The native resource
guide offers Workspace files when its contribution is installed, New terminal
when the dock owns terminal access, and More tools for other contributed
surfaces, commands and retained shells. List terminals and Refresh terminals
each read the current roster once. Closing More tools releases its roster.
Opening the guide or its menu, creating a shell and opening Files do not need a roster read.
Each guide uses the Session and generation of its containing pane. Hiding the
guide dismisses its menu, including when the terminal owner is unavailable.
The guide fills its tab body, centering the entries in the available space and
allowing them to scroll when the pane is short.
Each pane offers the add-tab button while it has room and holds no guide.
Opening the guide focuses the one in the destination pane or creates it there.
The guide has no Duplicate action. Moving it into a docked pane that already has a
guide focuses the destination guide and removes the arriving copy in one undo
step. Independent resource views, including Files, can still be duplicated.
Splitting the resource dock opens the guide in the new pane. One Undo removes
the split and its guide together. At the sixteen-tab limit the new pane stays
available as a drop target for an existing tab.
When a narrow right-hand pane cannot fit its tab and the dock-wide controls on
one row, the controls use a second row. The tab and its close button stay
reachable by pointer along with undo, redo, fullscreen and hide.
Individual terminal tabs attach one explicit terminal or consume its creation
lease. Navigation and terminal listing belong to the launcher; tabs neither
maintain a hidden roster nor issue list requests on mount, exit or termination.
Opening or returning to the selected writable terminal places keyboard focus
in its input. Background output preserves the user's current focus.
Only a visible, nonzero terminal viewport with finite measured cell counts can
request a resize. Hiding a retained host never resizes its remote shell.
Automatic resize diagnostics belong to the current running controller; a late
refusal after exit or loss of control does not replace the retained terminal
view with an error. Explicit action and output-read errors remain visible.
Unused space below a retained terminal grid uses the current surface theme.
The emulator adjusts low-contrast ANSI text against its current background
using xterm's minimum contrast ratio of 4.5.

Application extension restoration sends the ownerless application command;
Session extension restoration carries its current pane and generation. The shell
measures the viewport once and passes its dimensions to the resource slot. All
retained Session docks use that same snapshot for fullscreen and float bounds. Narrow
resource-overlay visibility is transient and never writes the saved desktop
visibility, including when opening a resource tab. Collapsing docked panes on a
narrow viewport preserves the focused floating pane and the root dock selection.
Keyboard resizing preserves the dock engine's minimum float size, including
End-to-fill when a restored float lies near the viewport edge.

The starting page has no resource column or resource-toggle action. A saved open
column is suppressed until a conversation is selected, without overwriting that
desktop preference. A selected external conversation can show its resource
summary; no Session dock or terminal is allocated for the starting page.

The first-use composer selects the Service-provided default workspace, or an
existing registered workspace when available. Its input remains usable before
model configuration; the model control opens setup when no model is available.
The workspace selector chooses registered workspaces and offers the directory
picker for adding one. Composer controls expose only implemented actions.
Completion hints appear only for an attached, available conversation: `/` offers
commands and `@` offers Agents. Draft and switching composers use a plain prompt.
Conversation headings and underlined tabs separate the transcript from execution
activity. User messages are compact right-aligned bubbles; assistant output uses
the shared conversation width with its actions beneath the reply.

Workspace rows use the active conversation's workspace to color the folder icon.
Conversation activity uses an accessible leading status mark: a muted animated
ring for running, a waiting mark or unread dot for those observed states. Unknown
is distinct from running; reduced-motion preferences stop the ring animation.
The sidebar footer has one More disclosure containing Settings and application
actions. Its menu supports outside/Escape dismissal and focus return.
Collapsing an active workspace records the device preference before requesting that its group close. An arriving navigation frame cannot reopen it during the command.

Hovering a registered workspace for 800ms, or focusing its title, reveals its
name, full path and recorded creation time in a viewport-bounded card. The row's
actions offer Rename and Delete workspace; Ungrouped has neither action.
The sidebar omits Ungrouped when the complete filtered conversation list has
no loose Session. A current loose draft keeps its group, and a partial list
keeps the group available for browsing.
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
Assistant action rows expose copy, source inspection and **Turn usage** for a
complete Turn whose conversational requests all reported token counts.
The panel identifies each contributing deployment and model when the complete
set of routes is available.
On hover-capable devices the latest user message and the latest closing assistant
response keep their action rows visible. Earlier rows reveal actions on hover or
keyboard focus without shifting the transcript. Touch devices retain visible actions.
An open disclosure stays visible. Source actions and fork forms mount when opened;
Verbose mode keeps its inline source actions visible.
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
Starting another turn or streaming new text preserves the existing statistics.
Metrics refresh when a completed turn becomes visible in the transcript; failed
reads and unfinished reductions offer an explicit retry in the statistics details.

The composer footer has separate Session statistics, Token usage and Context usage
buttons. Each opens one nonmodal, viewport-clamped panel above its own button.
The menu tint overlays an opaque theme surface. Statistics and composer menus
use CSS anchors for placement on supporting engines. Panels support
Escape/outside dismissal and keyboard focus restoration. Turn usage uses
the same panel skin. Token detail partitions inclusive input by reported cache reads; cache writes
are a supplementary subset of uncached input and reasoning is a subset of output.
Unknown subsets are not presented as zero. Session statistics use whole-log recorded model
time, accepted-to-terminal wall time, mean TTFT and decode throughput. The context
button's ring and percentage show the same rounded occupancy, capped at 100%.
Its accessible name includes the percentage and identifies it as context use.
The detail shows the last reported input and captured input capacity; system/tool/message
subdivisions are absent because the current metrics owner does not report them.

Models settings show configured provider cards with one inline editor. Each
Edit control names the deployment it opens for assistive technology. API key
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

After a confirmed composer model or effort change, the selection also becomes the
default for future conversations when the connected Host permits Settings writes.
The Session selection takes effect immediately. Background saves run in the order
the selections are confirmed. Other conversations retain their own selections.

The composer offers the same model/effort, add-input and access menus before and
after the first message. Menus mount their contents while open. Browsing the
model, access or Add menu leaves the starting draft unattached. Choosing the
current access mode dismisses the menu. A session-specific action opens one
draft Session and transfers the saved starting text without submitting it.
The empty conversation offers a draft download in the Add list. Once history
is visible, Session tools offers the conversation download and its cancellation.
Both controls use the same export as `/export`. Export uses the attached Session
generation, including in a protected investigation without a draft editor.
The Add list spans the composer,
keeps focus and the selection in its editor, and exposes its active option for
keyboard navigation. File references and
available Session commands share the list. Arrow keys cycle through enabled
options. Enter or Tab selects, and Shift+Tab or Escape dismisses. An unattached draft offers an
explicit Commands option to bind the Session and read its catalog. The image
file chooser opens on the user's gesture and binds the draft on file selection.
Concurrent controls share that operation; failed or replaced attachments retain
the starting draft. Model choices use the configured catalog and effort choices
use the selected model's declared profile. Selecting the current model retains
its reasoning effort. Selecting the checked effort closes the menu without a
command. An explicit level pins that level even when it currently matches the
model default. Choosing Default clears a pinned effort.
A changed selection uses the existing durable
model command and its uncertainty receipt. Model and access menus open above
their trigger, stay inside the viewport and restore trigger focus after selection.
Escape and outside interaction dismiss open menus.
The new-conversation composer offers the installed Agent presets next to its workspace.
Opening the menu reads the current catalog, including the effective default and
descriptions. Broken presets are omitted from selection. An explicit choice is
staged in the current blank surface and applied by Session creation. An attached
unpublished draft can change its preset in place. The Host reports whether that
draft is still editable. Published conversations display their recorded preset
in the header. The starting page resolves a custom default's display name on
first appearance. Conversation defaults displays the current roster when opened.
Each preset shows its description and selection state. A broken preset retains
its discovery diagnostic and cannot be chosen.

The starting composer reads the configured default effort and the exact default
model's Host-described profile during setup, before any menu is opened. Missing
or mismatched descriptions never borrow another model's capabilities. Preparing
an empty draft does not introduce a header row or move the composer.

Back to bottom is a 34 px button with a 16 px chevron, aligned to the right
edge of the conversation column with a minimum 32 px inset and 16 px above the composer. A settled reading
position more than 25 px above the transcript floor releases tail following
and reveals the button; the threshold includes one pixel of browser rounding.
Expanding a turn preserves the reading anchor without itself enabling this
control. Scroll, transcript updates and viewport resize recalculate visibility.

Access selection applies to subsequent queued messages from this browser, never
to an active Turn or a frozen retry. Read Only, Workspace Write and Full access
map to the typed sandbox request modes. Full access opens an acknowledgement
dialog with its checkbox initially unchecked. Cancel or Escape preserves the
current mode. Enabling Full access requires a checked acknowledgement
and preserves configured Session and Tool approval requirements. Steering keeps the active Turn's
policy. The add menu exposes supported image import, workspace file references,
conversation references, history and the Session's actual command catalog.
Picker lifetimes are distinct from the metrics and other dialog lifetimes;
closing a picker removes its modal and returns focus to the original editor.

Saved-input recovery summaries use at most 80 Unicode code points with whitespace
collapsed to spaces; summary truncation never splits a surrogate pair.
Recovery includes retained unsaved editors in the current endpoint/device namespace,
even when their first write failed. Read-only text follows the bounded-preview
policy in [persistence](persistence.md). Expired inputs expose an explicit
remove-and-release action for references before recreation; captures
never move to another Session.
Resolving a starting-draft conflict preserves edits made while storage is read.
Reference removal retains local ownership until durable cleanup reservation succeeds.


Optional trusted document plugins compose through `src/document-plugins.tsx`.
An inspected closed question can mount a read-only review accessory in the
matching live Session occurrence. The resource ticket, Session generation and
pending request must all still match. Native question controls alone own answers.
Their module graph is part of the immutable application bootstrap and shares the
workbench's React instances. At most 16 definitions register before the workbench
mounts; their composer, transcript, turn-tail and panel occurrences are ordinary
React children, independent of the remote MountTable's 16 document slots.
Each occurrence inherits its pane, Session and generation. Replacement or unmount
retires its actions; raw frame data never selects executable modules. Plugin
renderers preserve the Rust transcript unless their registered matcher explicitly
claims a block. Native text/Markdown remains the default. Original DSH modules
and service adaptation belong to the external plugin repository.

A failed optional document contribution renders a local diagnostic; a transcript
contribution also retains its native fallback. Its error boundary resets with the
Session occurrence and must not remove the required workbench presentation hosts.

Document provider elements keep stable identities across draft publication and
Session changes so native presentation hosts stay attached. Providers consume the
optional Session binding and retire their own occurrence state when its Session
or generation changes; individual contribution occurrences may remount.

File-preview code and Markdown fences use the retained-state highlighting scheduler
inside their independently admitted renderer bundle. Its queue and grammar instance
are separate from the main document bundle and obey the same bounds. Plain lines
appear immediately; preview retirement cancels pending highlighting.

Transcript tables with at least four columns use their natural width and scroll
inside the message. On hover-capable devices their scrollbar appears on hover or
keyboard focus. Tables with fewer columns and tables inside a quotation fill the
text column and wrap their cells.

Settled transcript equations use KaTeX with its bundled fonts, accessible MathML
and trusted commands disabled. The first visible equation loads the renderer and
stylesheet. Plain chats do not fetch them. While a reply streams, the equation
remains readable as TeX. Copying a formula from a selection preserves its TeX
delimiters.
