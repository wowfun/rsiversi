# rsi-terminal-ui

Scene protocol version 12 carries one base surface (application or Session) and
at most one redacted application overlay. A bounded Composer prefix selects the
inline model/effort list at the base editor rectangle; other overlays use a
centered dialog. The wire type is not recursive. Base
rendering preserves its geometry while a dialog is open; the returned View exposes
only the focused layer's bounded input, body and choice geometry.
The closed transcript role includes source-free local error annotations separately
from informational notices, preserving error color across linked and native renderers.
Session scenes carry a bounded client workspace display label separately from the
authoritative Session header; pure rendering never reads the environment.
Application screens carry bounded labels, field visibility, progress, receipts,
editor display and selection. Completion carries only labels, descriptions and a
revision, never action authority. The [interaction design](../terminal/docs/tui-design.md) owns visible layout and
information hierarchy. Hit maps are
validated and authoritative only with the matching acknowledged frame.
An application screen has no Session header or transcript source map. Help detail
sources are at most 8 KiB per page and wrap and scroll within the available body.
Detail scrolling is bounded by wrapped rows at the actual body width and height.
The acknowledged View area carries that body geometry; resize and repeated Down
never scroll past the final content page.
Secret input is masked by the resident application before capture and never
enters a scene.

Partial transcript pages use each model event's authenticated purpose kind.
Context-compaction output is an internal status block, distinct from assistant
answers even when the preceding model intent is outside the loaded page.

Pure terminal presentation: grapheme editing, bounded transcript projection,
Markdown styling, layout and source positions. The resident terminal application
supplies borrowed display data and owns every controller, cancellation token,
action reference, terminal descriptor and process hook. Menu input contains labels
and selection only; rendering does not acquire authority from menu contents.

Transcript retention is bounded independently from the viewport. Layout retains
two screens of row metadata and reuses unchanged block layouts. Viewport clipping
does not mark resident text as evicted. A lost scroll anchor starts at the oldest
retained content; only a live-tail view captures from the end. A rendered source
map is meaningful only for its matching Session and completed screen write.
Summary-row clicks resolve through that frame's source anchors to a retained
foldable block. They do not grant source-copy authority to decorative labels.
Request metadata orders after the latest received Fact of its effect; retained
content blocks preserve their request identity so older metadata backfill can
recover that position without changing source coordinates.

This library performs no terminal I/O and starts no tasks. It is shared by linked
and native presentation code. Tests exercise grapheme editing, retention, wrapping
and exact source mappings; terminal restoration and writer backpressure belong
to the resident application tests.

The opt-in Linux `deterministic_render_cost` test records ten 16/64/128-block
runs through the actual projection, editor and cached renderer at 120 × 40 cells.
`RSI_TUI_PERFORMANCE_REPORT` selects a new report file. Thread CPU and edit-to-cell
render time exclude PTY writes, terminal-emulator painting and display scanout;
they must not be compared directly with browser input-to-paint measurements.

The portable renderer accepts a closed scene source, in contiguous binary chunks
of at most 64 KiB. A scene contains a bounded transcript window (at most 512 KiB
of text), a cursor window of the current draft, and bounded menu/detail values.
Folded process windows are captured at the frame width. They carry exact head
and tail source slices plus the original hidden-row count and gap anchors, so
clipping cannot rename a middle window as the beginning of a process. The
renderer rejects a capture width mismatch. Known typed Tool summaries omit raw
argument/result previews until expanded. Shell calls retain a ToolCommand source
with byte offsets into the original command string. The 512 KiB text bound still applies;
no whole resident transcript or business action reference enters the scene.
The scene rejects a question index outside its request and invalid UTF-8 cursor
windows before rendering. Editor text and cursor mutate only through editor
operations; consumers receive read-only accessors.
The 128 KiB UI model names this source; large text is outside model JSON. The
renderer returns a complete binary cell frame and exact source map, never ANSI
commands. The resident writer validates dimensions, symbols, styles, source
membership and the attachment/presentation/revision tuple before publishing it.
Only the matching completed write acknowledges a frame. Replacing a renderer
preserves drafts, controllers and unresolved mutations; its private layout cache
may reset.
Linked and Portable adapters both validate the declared scene byte length before
decoding. A resident recovery frame reuses cells only from the same attachment,
presentation epoch and dimensions; it carries no source-map authority.

Each presentation Renderer owns its private body layout cache.
Restored blocks are matched by an indexed key and compared for exact content
and source mappings before reusing a layout revision. A wire revision alone is
not proof of unchanged bytes. Fold geometry has one owner in the layout module:
above five rows, retain two head rows and three tail rows, with one gap marker.
Viewport compression, drawing, copying and scrolling consume that same range.
The controller's scene capture separately caches compressed process windows so
unchanged folded bodies are not wrapped again on composer edits. Its LRU retains
at most 512 entries and 1 MiB, including copied window text and source mappings.
Keys bind the original block revision, width and summary mode; no Facts or source
leases are retained. A changed body or width recomputes that window.
Keys include the block's opaque content/source-mapping revision, width and
collapse state. A cloned historical projection retains its revision until changed;
a changed Session replaces source revisions; presentation replacement creates a fresh cache. Insertions, source-window eviction,
and backfill create a new revision. Titles and selection are projected from current
state; source anchors and hit maps always resolve through the current pieces.
The cache retains at most 512 entries and 32 MiB of owned text, compact row ends,
Markdown style ranges and keys. Least recently used entries are evicted under
pressure; the currently calculated block is a separate transient bounded by the
existing 256 KiB source-window limit. Eviction affects recomputation cost, not
visible content or selection semantics. Each redraw retains only two screens of
visible row metadata; the writer's complete-frame acknowledgement remains the
sole publication boundary for hit maps.

Frame source validation indexes retained pieces once per frame and checks exact
source offsets, including folded gaps. Cached live-tail row enumeration visits
only a bounded tail per block; collapsed interiors are skipped by row range.

The composer shares one sanitized draft between measurement and rendering. Body
graphemes write directly to the cell buffer; row layout and source hits still use
the same grapheme boundaries.

Local transcript annotations have no durable source or submission authority.
User time labels are derived from accepted Facts. Model metadata is visible only
for a completed final answer; the resident retains all requests for inspection.

The resident supplies visible-turn identity and explicit clock samples. Tool
motion requires the matching active turn; completion duration is frozen from
Facts. Fold focus carries a retained source and bounded screen row independently
of copying authority, so annotations and metadata do not relocate clicked
summaries. Explicit navigation releases that focus. The renderer never reads a
clock or owns an animation task.

Process outcomes cross the viewport as a closed enum, independently of fold
state and display titles. Terminal outcomes require completed, non-running Tool
or reasoning blocks. Request outcomes survive reverse history backfill through
the retained request projection; partial history cannot imply success.
