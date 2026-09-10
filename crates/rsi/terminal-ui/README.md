# rsi-terminal-ui

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

This library performs no terminal I/O and starts no tasks. It is shared by linked
and native presentation code. Tests exercise grapheme editing, retention, wrapping
and exact source mappings; terminal restoration and writer backpressure belong
to the resident application tests.

The portable renderer accepts a closed scene source, in contiguous binary chunks
of at most 64 KiB. A scene contains a bounded transcript window (at most 512 KiB
of text), a cursor window of the current draft, and bounded menu/detail values.
It does not serialize the resident transcript or its business action references.
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
