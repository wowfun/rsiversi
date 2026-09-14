---
name: Transcript-first terminal information layout
---

## Problem

Repeated role names, fixed header rows and decorative indentation consume scarce
terminal space and contaminate copied prose. A permanently expanded process log
hides the response, while a tail-only preview loses the command's starting context.
A durable child must also be reachable without confusing navigation with execution.

## Decision

The attached application gives row zero to Transcript, puts actionable transient
work above Composer and keeps priority-shedding information and contextual keys
below it. Reserving the keyboard row after the first response prevents the input
from shifting as conversation state changes. User messages
use a subdued band and small chevron; copying returns original source text.
Timestamps and final-answer metadata sit immediately below their message without
background. Blank rows separate Thinking from adjacent metadata and assistant
prose, keeping the process visually distinct from the answer. User bands
keep their lower edge adjacent to the timestamp so the time belongs visually to
the message above it. Intermediate tool requests retain their
inspection data without repeating usage rows in the conversation.
Process folds retain their starting context and latest output, and exact source
anchors bind selection to the acknowledged frame. Human-readable typed Tool
summaries are the default; raw source details require an explicit action.

The current [interaction reference](../../../../crates/rsi/terminal/docs/tui-design.md)
owns geometry, key behavior, source-copy limits, feedback priority and navigation.
The [presentation contract](../../../../crates/rsi/terminal-ui/README.md) owns the
portable codec, frame acknowledgement and separate bounded caches. Home and setup
remain usable before Session creation. Child navigation attaches its existing
Session and restores independently bounded drafts and reading state.

The pinned DSH TUI `Chat.tsx` renders StatusLine after PromptInput but retains a
startup header in history. Codex's `history_cell/session.rs` and `bottom_pane/mod.rs`
similarly separate a startup card from a lower status line. Neither proves that
all headers are absent. pi's `user-message.ts` uses a background Box with nonzero
default padding; RSI keeps padding decorative and outside source-copy authority. Grok's `status_line_policy.rs` conditions recurring work on the active
agent and visible frame. These observations support separate choices rather than
an asserted upstream-equivalent layout.

## Alternatives considered

Moving every former header field permanently below Composer merely relocates the
same noise. Role prefixes make literal terminal selection include UI decoration.
Using only a process tail discards useful context; serializing its whole hidden
body can exhaust a portable scene before visible summaries are included.
Always-live child observers and output subscriptions spend resources on hidden
information. Explicit attachment and focused output details preserve user intent.

Putting model and effort choices in the same centered dialog as setup forms
moves typing away from the Composer for a lightweight selection. Grok's
`xai-grok-pager/src/app/agent_view/render.rs` places the slash dropdown relative
to the existing prompt, and `views/slash_dropdown.rs` supplies bounded gray rows
and their hit rectangles. RSI uses that inline interaction for model and effort
selection, while retaining a separate focused dialog for configuration and
inspection. Neutral gray bands communicate focus without giving ordinary work
the visual weight of an alert.

Grok's `slash/commands/model.rs` advances from a model to its declared effort
choices before applying the pair. Keeping that choice in one bottom input and
restoring the model filter on Escape prevents partially applied selections.
Its `scrollback/blocks/session_event.rs` separates local status annotations from
conversation messages; RSI similarly adds acknowledged selection feedback without
creating a Fact or provider request.

Source-only viewport anchors cannot preserve a clicked process below source-less
annotations or metadata. Retaining the acknowledged process source and screen row
preserves its position; explicit scrolling releases it. The End hint shares the
existing footer rather than reducing the viewport after a click.

Metadata and padding share byte boundaries with message text but do not name a
distinct scroll position. Navigation therefore chooses source content rows and
skips decoration, stopping at process boundaries when scrolling upward so a short
viewport cannot skip the summary. Copy and scroll share retained source identity
without treating decorative cells as text.
The final forward scroll uses the tail layout when source-only anchors cannot
represent the remaining metadata/padding rows. Once that tail fits, further
forward scrolling is a no-op; a short conversation and a pinned fold do not move
merely because their content contains another selectable source row.

A tool's bounded argument summary cannot serve as its expandable source: it
normalizes whitespace and clips long commands. Shell calls therefore retain a
separate exact command-string source, selected from their durable intent or
rejection by the shared conversation library. This preserves shell copy semantics
without making the TUI decode or manufacture offsets into escaped JSON. Existing
source window and retention limits still apply.

Codex's `tui/src/exec_cell/render.rs::command_display_lines` uses `Running`/`Ran`
and an outcome-colored marker; `history_cell/mcp.rs` uses `Calling`/`Called`.
RSI adopts action-oriented titles while keeping its disclosure glyphs and raw
output copy semantics. This avoids repeating a generic `completed` field between
the tool name and arguments. The product picker owns `/model` and `/effort`;
exposing its structured Session command as a second slash command requires users
to know an internal JSON shape without adding a product action.
Local errors carry a distinct presentation role, rather than inferring severity
from a leading punctuation character or changing their durable meaning.

Psychevo's `specs/210-pevo-tui/sessions.md` and
`crates/psychevo-cli/src/tui/support/motion.rs` distinguish active ownership from
historical unfinished work and use eight spinner frames at 120 ms. RSI supplies
confirmed visible-turn identity and time to its pure renderer. History alone
cannot keep a spinner alive; completion freezes tool duration.

Success and failure color only the process marker. Thinking derives its result
from its own model request, while Tools retain their typed result classification.
Keeping the outcome independent of fold state and serialized titles prevents
completed-but-failed work from appearing successful and keeps portable rendering
consistent with the resident projection.

## Consequences

Narrow terminals omit lower-priority status fields; details preserve access.
A folded gap cannot be silently copied as contiguous source. Expanding or an
explicit source action is required. User colors and final selection colors work
within the basic terminal palette, and background filler has no copy authority.

Tests compare full and serialized folded scenes at 110x35, 80x24, 42x12 and 28x9,
including hidden source anchors and text exceeding the scene window. Cached
capture avoids rewrapping unchanged process bodies on composer edits. Native
renderer replacement, acknowledged hit maps and real local/daemon PTY navigation
exercise the owning boundaries; screenshots and live-provider runs are separate
execution evidence, not substitutes for source invariants.

The resident input loop owns the bounded snapshot because it borrows live editing
and navigation state. Encoding and linked rendering run on blocking workers:
returning an already-computed result inside a Future does not defer CPU work.
Home and attached Sessions share the same render job, preserving the existing
attachment and presentation fences. Layout reuse indexes restored block keys but
compares their actual text and source maps; a portable revision token cannot prove
that separately decoded content is unchanged. One layout-owned fold range drives
compression, drawing, selection and scrolling.
