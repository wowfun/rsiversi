# rsi-session-files-ui

Successful `present` Tool blocks also expose recorded file cards. Their actions
contain only intent/result sequence coordinates and a file index. The renderer
reads durable Facts and validates the exact Tool identity and result format;
opening then uses the current human Files binding. File declarations preserve
paths and descriptions, not historical bytes or Tool execution authority.

An ordinary UI contribution supplies a workspace browser over Session Files.
The TUI consumes closed cards, inputs, text/code and buttons; the GUI also
consumes the rich model described below.
Neither adapter implements Files authorization or reader semantics.

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
also have their complete hex representation. Standard source/hex content remains data and grants no execution or write authority.

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

## Rich file previews

The GUI consumes a named Files preview model through the existing presentation
source channel; standard text/hex remains the terminal and over-budget fallback.
Files and recorded `present` cards share the same browser owner. Source/Preview
uses the same opened version; only explicit Refresh opens current bytes. The
preview preparation is keyed by the opened Session target and file token, not the
action revision: paging and display actions retain resource tokens and prepared
bytes. UTF-8 preflight retains code bytes for subsequent source pages, which still
validate the captured file's authority and version.
Complete text previews are limited to 1 MiB; images and complete document resource packages
are limited to 32 MiB and 32 named sources (main, rendered document and at most
30 resource entries). Every source retains Session/Header,
file version and presentation revision checks. Each extra document resource is
limited to 4 MiB; missing resources produce bounded diagnostics and image alt
labels. Unreadable local resources are omitted from the package; their references
become inert about:blank URLs without discarding the rest of an HTML document.
Present resources that fail byte admission still reject the package.
Prepared resources retain their owner through cancellation cleanup. Closing cancels reads and disposes
Blob URLs; viewing an image never imports it into persistent Media storage.

File Markdown has its own closed rendering contract including tables, fenced-code
languages and workspace-relative images. Raw HTML remains literal there. HTML
preview instead runs in an opaque sandbox allowing scripts, using separately
served immutable bootstrap documents with distinct CSP. Approved local resources
are transferred as bounded data, never as file-reading or native capabilities.
Relative references stay inside the Session workspace. CSS references are parsed
and bounded; modules, development servers and multi-page navigation are outside
this single-page preview contract. External resources default off; explicitly
enabling HTTPS applies only to the current frame, and mode changes replace it.
The control states that the document and remote scripts may send the previewed
contents and bundled resources to any HTTPS server. It conveys network trust,
not just permission to display remote images.

Embedded data image/font references join the same bounded resource package as
workspace files; raw data images are excluded by the sandbox response CSP.
Every bundled resource is checked for raster signatures regardless of its MIME
label before creating a Blob URL. Before creating image Blob URLs, the document
renderer rejects more than 16,777,216 pixels or animated raster files.
One preview admits at most 33,554,432 pixels across distinct local/embedded
resources recognized by that image gate, including CSS references. This limits
admitted source dimensions, not total browser memory, repeated DOM instances,
text-labeled SVG or script-generated/remote images.
Non-image resources admit UTF-8 text and supported font container signatures;
unrecognized binary blobs are rejected instead of relying on a supplied MIME label.
This also applies to bundled Markdown and HTML images. Malformed or unsupported headers produce a
diagnostic without invoking the image decoder; the existing paged text/hex view
and its actions remain usable. Image-context SVG is limited to 256 KiB before XML
parsing, 2048 DOM nodes, depth 32 and bounded intrinsic raster dimensions. Reuse
through `use`, SMIL animation, embedded stylesheets, embedded images and foreign
DOM are declined; source/hex remains available. These checks admit supplied resources, not all browser allocations:
arbitrary HTML scripts can generate their own Blob images or allocate memory,
and HTTPS opt-in admits remote responses outside this gate. This is not a CPU
or memory sandbox for scripts or complex SVG.
