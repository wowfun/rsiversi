# Terminal interaction design

The interface shows information when it clarifies intent, enables an action or
adds a distinct fact. It does not repeat role names, idle status, zero counts or
transport ownership. Keyboard controls belong in the focused surface's footer.

## Editing and actions

Enter, Ctrl+S and Ctrl+Enter submit NextTurn. Shift+Enter and Ctrl+J insert a
newline; Ctrl+O sends Steer. Paste only edits. Ctrl+P opens actions, Ctrl+Y copies
selection, and releasing a mouse selection also copies it. Ctrl+R recalls this
Session's local inputs. Copy with no selection leaves the interface unchanged.
Ctrl+Z undoes, Alt+Z or enhanced Ctrl+Shift+Z redoes; a paste
is one edit and editing after undo replaces the redo branch. Form fields and
question answers accept with Enter.

With Composer focused, Ctrl+C cancels the current Turn and this client's accepted
pending input while preserving the draft. In setup or help it closes that layer.
Escape closes the focused layer or selection; Ctrl+D exits from an empty editor.
Alt+Enter belongs to the terminal. Application commands and masked setup input
follow the [terminal contract](../README.md).

Tab completes a slash-name prefix from current Session descriptors; ambiguous
matches open choices. Reads never execute a command, and changed drafts or
attachments reject stale results. Outside completion, Tab selects the next block
with source text and toggles its preview if foldable. PageUp/PageDown scroll; End resumes live following. While browsing, the existing
shortcut row offers End without adding a dock row or moving the Composer. Opening
another detail resets its actions. Left/right page through exact source or output
reads. Enter opens a card's actions; field editing keeps the editor visible.

## Attached layout

Use terminal-default background, neutral gray text and subdued gray selection
bands. The Composer uses a top and bottom rule with open sides. Its leading
chevron aligns with the user-message chevron at column zero; their text starts
at column two. Tool summaries, routine running
work and code styling do not use bright accent colors. Copy selection stays
highlighted; errors and confirmations remain explicit in text. Inline choices and command completion use
the same full-width gray rows, with a stronger background and weight for selection.

Transcript starts at column zero, row zero. There is no persistent header. The
footer information row starts with the selected model and effort separated by `·`; context, usage, configured cost and directory are added in that priority
order while space remains. The model
selection describes the next Step, not a rewrite of previous request metadata.
Declared default effort is displayed without converting it to an explicit choice.
When no specific effort is declared or selected, `default` denotes the provider
default without claiming a specific reasoning level. Session identity is available
in Session details; a child Session retains the actionable return-to-parent arrow.
An unavailable current route is labeled and keeps its model-selection action.
Only the displayed model/effort span is clickable as that action; directory text
cannot trigger it. An empty conversation leaves Transcript blank. Keyboard
shortcuts occupy the last row, below model and workspace information, with a
stable two-row footer before and after the first message. Narrow terminals
shorten the hint to help. The recurring hint omits Enter submission. Workspace paths inside the local client's
home directory use `~` for that path component prefix; this is a display label
only and never changes the Session workspace path.

A conditional dock immediately above the Composer contains running work, pending
questions/approvals and actionable feedback. It occupies no rows when empty.
Background metrics, history and inspection reads do not add progress notices.
Reaching the beginning of retained history is a silent scroll boundary.
Scrolling toward newer content stops when the transcript's last row fits inside
its viewport. A final scroll step is clamped to that boundary, so the last answer
cannot keep moving upward and expose empty space. Further wheel-down/PageDown
events leave the displayed frame unchanged; wheel-up/PageUp still navigate older
content. A fold's pinned position is released only by an effective scroll.
At 28 × 9 the layout reserves three transcript rows, three composer rows and one
information row and one shortcut row. Remaining height goes
first to at most three dock rows, then up to
six wrapped composer text rows. Invalid terminal dimensions show resize guidance
without mutating drafts or view state. Informational feedback is replaced on the
next action or edit; errors persist until dismissed or their cause is resolved.
Do not add dock receipts for ordinary submission, successful model/effort
selection, prompt recall, field edits, dismissed dialogs, or completed
question/approval replies. Their resulting content, selection or pending count
provides feedback. Submission IDs and delivery details stay in input inspection.
Setup receipts stay in setup; closing with a save pending or completing a save
after close still reports the outcome because the normal follow-up was stopped.
Successful copy and OSC52 dispatch are silent. Actual copy failures remain explicit;
a silent OSC52 dispatch does not claim confirmed clipboard delivery.
Model and effort choices open as a bounded inline list immediately above the
Composer. Their command prefix and filter use the existing bottom input rectangle;
the background draft is retained until selection closes. Filtering changes the
list height upwards, never the Composer or footer position. The list shows at
most eight choices, keeps the selected row visible and highlights it with a
subdued gray background. Zero matches remain explicit. Contextual keys replace
the bottom shortcut row; no second input or dimmed conversation is introduced.
Loading and validation feedback stay within the inline selection area.
Up/down selects. Enter or Tab on a model advances to its effort panel, retaining
`/model <selected model> ` in the bottom input. Even a model with no declared
effort offers Provider default; unsupported levels are never invented. Enter on
an effort applies the pair; Tab on an effort fills its label without applying.
Ctrl+S also saves the startup default. Escape from effort returns to the model
list and its filter without applying a partial selection; Escape again restores
the prior draft.

Provider setup forms, help, action menus, details, live
questions and field edits use one fullscreen dialog layout. The conversation and
its Composer remain in place underneath, visibly inactive. Dialogs have a centered
border and their own labelled input above the list/body. Filtering or changing
result counts never moves the dialog or its input. Enter/escape controls and
feedback occupy reserved rows inside the dialog; titles do not repeat them.
Dialog size depends on terminal dimensions, not item count. On small terminals,
the dialog fills the available screen and keeps at least one body row, input and
exit controls visible. Lists and details scroll within their actual body rectangle.

The focused dialog owns input and mouse actions. Session menus and details retain
Ctrl+P actions and Ctrl+D exit; Ctrl+D still requires an empty Composer.
An editable dialog field owns its editing keys. Its acknowledged hit map
contains no background transcript, footer or completion actions. Mouse choices
are bounded in both columns and rows; input, borders and outside clicks do not
select a row. Paste and unrelated submission/cancellation keys cannot modify or
submit the background draft, including a field underneath another menu. Ctrl+Y
copies a focused detail or selected Session ID, never a hidden layer. Escape returns to the prior layer and retains that
draft. A read-only dialog has no text caret. Reopening/resize invalidates old hit
geometry. The unattached home is a full-height page with bottom-anchored input;
its setup/help dialogs follow the same overlay contract without inventing a
Session header. Home action/session lists carry their own title, controls and
feedback inside the dialog. Ctrl+C closes a focused dialog; at the unattached
home page it exits.

## Transcript

User messages use a subdued gray band (235), a small leading chevron and a
half-row visual inset above the text. There is no bottom padding row: metadata
occupies the very next terminal row after the last source row, outside the band.
Time is the first
metadata item below each user message and final assistant answer, in 24-hour
`HH:mm` format without a date. User time comes from acceptance; assistant time
comes from response completion, never the rendering clock. Times use UTC.
The chevron, timestamps and band padding have no source-copy authority. Continuation lines share the content column;
copying returns original text without UI indentation. Assistant prose remains
at column zero with terminal-default colors and no role label. Source indentation in code and lists remains.
Selection applies Black on Cyan after all other styling. Background filler has
no source hit. Markdown is enabled by default for assistant prose and expanded reasoning;
`/markdown [on|off]` switches to original text for this process. Headings, emphasis,
quotes, lists, task items, code, links and tables have semantic layout. Narrow
tables use field records; HTML is literal and images show alternative text.
Incomplete streamed constructs remain readable. Display runs retain original
source ranges; copying reads those sources and never serializes parsed Markdown.
Decorative list/table glyphs and soft wraps have no copy authority. A final
synthetic Markdown block separator does not add an empty transcript row; the
transcript owns inter-block spacing. Explicit source newlines remain intact.

Thinking and tools have concise descriptive summaries. One blank row separates
Thinking from preceding metadata and from the following assistant prose. When
expanded, the lower blank row follows its reasoning content. Expanded reasoning
uses the current Markdown mode; code preserves source whitespace.
Only the existing process marker carries outcome color: bright green (ANSI 10)
means a successful model request or Tool result; bright red (ANSI 9) means a failed request, failed command,
Tool failure or rejection. Thinking follows its own request's terminal event,
not a later Tool or Turn outcome. Closing a reasoning content block alone does
not establish request success. Cancellation, output limits and content filtering
keep a neutral disclosure marker, as does incomplete history. Matching live work
retains the neutral animated spinner. The collapsed `▸` and expanded `▾` markers
keep their shapes after completion. Titles and source text
keep their normal colors. Outcome markers do not change the summary's click
target or the ability to expand and collapse it.
Clicking a summary row
toggles its retained source rows while retaining the visible reading position;
it never scrolls the clicked summary to the top. Expansion pauses live following
so added rows cannot push the clicked block out of view. A fold retains the clicked
summary and its screen row even when source-less annotations are above it;
explicit scrolling or End releases that position.
Scrollable positions resolve through source-bearing content rows, not user
padding or metadata. Scrolling skips source-less annotations and advances through
expanded output, including wrapped lines; scrolling upward uses visual source
rows at the current width. Stale frames cannot navigate another retained projection.
The summary itself is
not selectable source text. Only the acknowledged frame and a still-retained source anchor identify
the block to toggle. Thinking is shown only when reasoning content was received;
requests without reasoning content do not gain a placeholder.
A running folded process
shows the first two wrapped source rows and the latest three, with an explicit
hidden-row count between them. Completion reduces Thinking to one summary.
Manual expansion persists. Normal prose remains fully expanded. Known typed tools
show action-oriented summaries until expanded: `Running/Ran <command>` for shell
execution, `Listing/Listed <path>` for directory listings, `Reading/Read <path>`
for file reads, and `Calling/Called <tool>` for other calls. Prepared calls use
an imperative action; failures and rejections are explicit. Successful summaries
omit the redundant `completed` label and dot separators. Arguments remain visible
when known; raw JSON adds no additional default preview.
Shell calls retain the full command as source content even when the one-line title is
truncated. Expanding a shell call shows its wrapped original command before the
result; copying those rows preserves command bytes rather than JSON escapes.
The command/result boundary starts a new display row without appending a newline
to either source.
This also works before the call has produced output. Commands use the existing
bounded source windows and exact-source paging for content beyond retention.
Tool summaries use typed arguments and lifecycle facts; unknown tools have bounded fallback
previews and exact paged details. Actionable system events appear in history;
request prompts and configuration belong in the inspector.

Only the final assistant answer displays model metadata, after its response
content and completion. Tool calls, tool results and intermediate model responses
carry no metadata rows. The final label describes that request, not an inferred
sum of the whole Turn; per-request and aggregate usage remain in the inspector.
Metadata immediately follows its message or user band; one blank row after the
metadata separates it from the next message.
Metadata starts with a dot
and time, uses gray 247 text and terminal-default background. Model, effort, tokens and elapsed
time come from request Facts; unknown metrics stay unknown.

Local status events appear as quiet, non-foldable transcript annotations; error
annotations use bright red (ANSI 9) text and remain explicit and actionable.
Model/effort changes are recorded only after the
selection command succeeds. These annotations are presentation-only: they have
no Fact source, are never submitted or included in model context, and are bounded
to the current attached view. Running work and live questions remain in the dock.

Copy operates on source positions with a 4 MiB all-or-error bound. Selection
across folded gaps requires expansion or explicit full-source copying. Exact
source pages are bounded to 256 KiB. Scrolling and resize retain a source anchor;
selection pauses following, End resumes. Only the source map acknowledged with
the displayed frame and matching attachment generation authorizes mouse actions.
Visible copy validates both endpoints and folded gaps from that acknowledged
frame; streaming cannot reinterpret an older selection through a newer fold.
Selections outside the displayed source map use the explicit source-copy action.
If resize or folding hides a saved source anchor, its process summary or gap
stays at the top; expanding restores the same source position. Folded scenes
retain head and tail slices from the whole retained block and its original hidden
row count, even when their combined process text exceeds the 512 KiB scene window.

## Model selection and navigation

`/model` is the product picker and `/effort` changes the current model's effort.
The structured `model-selection` Session command is used internally by the picker;
it is excluded from TUI completion, help and command menus. Typing that internal
name is rejected locally with guidance to `/model` or `/effort`, without sending
a message or executing a Session command.

Successful model discovery shows the selectable models without a persistent
success or capability-verification notice. Selecting model/effort updates the
footer and adds one quiet local annotation after acknowledgement.

`/effort` opens the current model's declared effort choices directly, highlighting
the current selection. It makes no model request. Provider default clears an
explicit choice; a model with no declared choices offers only that default.
Before a Session exists, choose a model first. `/effort` accepts no arguments.
`/model` reads the selected adapter's declared effort choices. Enter updates the
current Session; Ctrl-S additionally saves startup defaults. Unknown routes need
explicit capacity. Manual model entry also offers an optional comma-separated
effort declaration and a default selected from those exact IDs; blank preserves
the provider declaration. These are configuration claims, validated by the
adapter before publication, not capability discovery or a test request.
Selections are durable Session state captured before each Step. A running call
and its retries keep that captured selection; subsequent Steps and Goal requests
use the newest selection. Explicit whole-Turn API overrides take precedence.
Ordinary TUI, GUI and Web sends supply no model override.

Subagent actions attach to the child's actual Session without executing it.
Returning to the parent restores its draft and view state. Sending to a child
uses normal resume preparation and completion delivery; finishing that work can
wake its parent. An unresolved send prevents switching. Parent
progress uses received messages, completion and tree activity without subscribing
to every child. Retention follows the [controller budgets](../README.md#controller-lifecycle-and-retained-state). Forked selection begins from the actual model
request that produced the spawning tool call; a separately selected child model
resets effort to that adapter's default.

## Task state and diagnostics

The task dock shows Todo state with unfinished items as completion count and the
current task. Completed-only lists occupy no dock row. Multiple active items
remain explicit. The Tasks action opens the full
ordered list; Escape returns to the compact dock. Empty state occupies no row.
Todo replaces the generic working row when it already explains the current work.

Todo is a typed Agent domain with whole-list `todo_write` updates, at most 64
items and 512 bytes per item. It permits multiple active items and persists until
cleared. Tool success and the resulting state publish atomically. Forks start
with an empty list. The UI reads and folds this state without a second authority.

Metrics are bounded forward reductions of durable facts at a fixed watermark.
Session totals exclude inherited and child attempts; tree totals are explicit
and disclose incomplete traversal. Context shows only the last successful
conversation request's measured input against its effective capacity after the
output reserve, labeled as the last request. Relevant model/configuration or
summary changes invalidate that comparison.

Request inspection uses bounded evidence stored with ModelIntent. Configuration,
system instructions and tools may reference the same section of an earlier
inline intent in the same Session; references never chain. Unavailable evidence
has an explicit reason. It contains no credentials, raw HTTP or duplicate media.
Live job output uses a read-only peek that neither reports nor acquires the job,
and never extends its original execution scope. Only one visible focused job is
polled, at most four times per second, with cancellation on close.

Cost uses a configured, Session-frozen price table and checked fixed-point
arithmetic. No configured table means no cost label. Missing prices or usage are
explicit in details; unlike currencies are never summed.

## Ownership and evidence

This document is a current-behavior reference. The [application contract](../README.md) owns startup, login, controller receipts
and terminal lifecycle. The [presentation contract](../../terminal-ui/README.md)
owns Scene validation, bounded caches and source-map publication. Individual
architecture decisions live under Agent Notes, with independent lifecycles.

Source comparisons use pinned local checkouts: DSH TUI's dock and process
summaries, Codex's bottom pane and thread navigation, pi's user-message styling,
and DSH Web's Step-scoped model settings. These are evidence for choices, not a
claim of feature or behavior equivalence. The [development guide](tui-development.md)
defines capture and isolated execution; Linux deterministic, PTY, visual and live
provider results are recorded separately.

## Activity motion

The fixed bottom information row carries a subdued turn spinner and elapsed time
while the visible Session has a confirmed active turn. It may show `0s`. All
activity uses the eight frames `⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧`, advancing every 120 ms.
Tool headers reuse the spinner in their existing leading cell and show elapsed
time at the right only from one second onward. Completion freezes duration and
stops animation. Historical unfinished tools without the matching active turn
are interrupted evidence, with no live timer or spinner. Rendering receives
explicit clock/lifecycle data; it neither reads a clock nor starts animation tasks.
