# rsi-session-files-ui

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
