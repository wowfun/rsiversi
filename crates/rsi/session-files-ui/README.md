# rsi-session-files-ui

Successful `present` Tool blocks also expose recorded file cards. Their actions
contain only intent/result sequence coordinates and a file index. The renderer
reads durable Facts and validates the exact Tool identity and result format;
opening then uses the current human Files binding. File declarations preserve
paths and descriptions, not historical bytes or Tool execution authority.

An ordinary UI contribution supplies a workspace browser over Session Files.
Web and TUI consume its existing closed cards, inputs, text/code and buttons;
neither adapter implements Files authorization or reader semantics.

`FilesUiTargetFactory` belongs to the actual Session surface. It depends on that
surface's controller, the Session service and the connected Files client. It
owns one optional opened snapshot, one nonqueued action slot and a monotonically
fenced, process-unique view revision. Replacing only the Files provider cannot
retarget an old action to a new browser whose Session controller stayed alive.
Different panes never share a browsing cursor, including
when they display the same Session. It creates no observer or additional surface.
The application must isolate `FilesBrowserContract` with its other surface Local
contracts. `FilesUiFactory` contributes the browser menu and actions independently.

Each action obtains the current actual Header before I/O. Page requests carry
only a view revision, offset and display choice; revisions and offsets use decimal
strings across the document so JavaScript cannot round large integers. Retained tokens and Session
targets remain Rust-owned. Paths are workspace-relative. Directory entries carry
exact byte paths so non-UTF8 filenames remain selectable. User-entered paths are
bounded to 4 KiB; discovered paths retain the Files protocol's full bound.
The UI requests 16 directory entries or 4 KiB of file bytes per page, within the
domain's hard limits. Text uses the Tool protocol's byte sanitizer; a separate
hex view shows exact bytes. Display names are bounded previews; selected paths
also have their complete hex representation. No content becomes instructions,
HTML, a process command or a write operation.

Paging reuses the opened snapshot. Changed/unavailable results offer explicit
refresh; they never silently reopen or retry. Closing an in-flight detail cancels
its read. A closed inspector may retain one snapshot for the actual surface's
remaining lifetime, bounded by Files' fixed lease. Opening another item,
refreshing, explicit release or surface retirement attempts early release.
Remote disconnect or a lost open response can prevent early release; the server's
fixed token lease and generation cleanup remain authoritative. No permanent
remote cleanup is claimed from a disconnected presentation. Retirement cancels
and drains the local action before dropping its state.

Tests cover revision/capacity fences, paging, exact bytes and paths, refresh,
read cancellation and ordinary target retirement. Actual Web/TUI visual and
transport evidence belongs to standard-product fixtures.

The composer file picker shares this same surface browser and nonqueued slot.
Its finite native interface exposes bounded directory choices and byte previews,
never tokens. Open selects a new snapshot; page and release require its exact
revision. Selecting a file inserts only a canonical workspace-relative locator:
`@"path"` uses JSON quoting for valid UTF-8; `@path_hex:...` preserves other bytes.
Preview, paging and insertion are separate human actions. Neither adapter loads
file contents into an input implicitly. Closing or replacing the picker cancels
its waiter; the existing Files lease still bounds a lost open response.
