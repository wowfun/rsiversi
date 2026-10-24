# rsi-web-assets

`VerifiedWebAssetsFactory` admits an independent bundle using its own bounded
receipt and bootstrap hashes. It does not compare against a core build family.
The package admission owner separately verifies distributor signatures and
required wire interfaces; a local directory conveys no distributor provenance.

WebAssetsFactory is an ordinary native provider of immutable HttpAssets. It reads
an explicit absolute bundle directory and a closed list of flat filenames at
activation. The compiled [bundle manifest](bundle.json) owns the fixed bootstrap set, each
file's production stage, and initial renderer filenames. Rust admission, the
frontend producer and development proxy consume that same source. It is build
input, never a receipt- or renderer-supplied override. Only HTML, JavaScript, CSS, WASM, JSON, PNG and SVG
extensions are accepted; names contain ASCII letters, digits, dot, dash or
underscore and cannot begin with dot. At most 128 files, 128 bytes per name and
64 MiB of aggregate retained file capacity are admitted. The root document must
be index.html and is also served at `/`.

The native `check_bundle` example accepts one absolute bundle directory and admits
the default bundle through this provider in an isolated Host, reports retained
bytes, then shuts it down. Build callers use it instead of copying admission policy.

The application frame policy permits the two preview documents and the `/downloads/`
Service Worker navigation prefix. Firefox also checks the synthetic navigation
against the exact `/download-worker.js` source, which is explicitly permitted. Download responses are owned by the client worker,
not by an HTTP business route or the asset provider.

On Unix the directory is opened no-follow, then files are opened relative to
that retained directory descriptor, no-follow and nonblocking. Only regular files
are read, with exact metadata length and EOF checks. Configuration may traverse
operator-selected parent directory aliases; the final bundle directory and its
file entries cannot be symlinks. The portable fallback rejects observed links
but does not claim Unix descriptor-relative race guarantees.

One tracked preparation worker reads serially, checking retirement between files
and read chunks. Its byte reservations precede allocation. Withdrawal cancels
that worker and waits for it; a blocked OS read can exceed cleanup's deadline and
must not be reported as successful cleanup. Publication occurs only after the
complete bundle validates. Withdrawal fences escaped lookups; outstanding asset
bytes retain their leases until delivery ends. HTTP request handling never opens
the filesystem or sees subsequent file edits. The plugin remains RestartRequired.
Within that activation, `WebAssetControl` stages and compare-and-swap publishes
renderer-only generations through the same `Arc<HttpAssets>`. A publication
changes no listener, Worker or bootstrap bytes.
`stage_verified` applies the same independent bundle receipt checks as initial
product admission before offering a candidate. A source package publication
may use it without enabling filesystem polling.

`ui-renderers.json` declares the renderer ABI, exact model schemas, requested
bound-host capabilities and SHA-256 for every entry, stylesheet, WASM file and
lazy import. The owner validates the complete graph before publication. Bootstrap
files and every file outside either generation's renderer graph must retain identical
bytes and document policy
in both generations, including files moving into or out of a renderer graph;
changes require an application restart. Model data never selects executable URLs.

An authenticated application acquires a complete generation lease before fetching
its manifest or modules. All renderer URLs include that generation's exact digest.
Current and old renderer generations remain addressable only while explicitly
leased. Internal staging references and escaped response bytes grant no lease; a missing generation never
falls back to current bytes. Unversioned renderer-file access is unavailable.
Static bootstrap assets remain readable before authentication. Authentication and
remote lease admission belong to the consuming API, not this filesystem owner.

One shared 64 MiB ByteBudget covers current, retiring and candidate files plus
escaped HTTP response bytes. A generation may occupy the entire cold-start bound;
hot publication additionally requires overlap capacity. There is one current,
at most one retiring leased generation and one candidate. A second retiring
generation or insufficient capacity rejects publication while current keeps
serving. An unleased current generation may be replaced while the existing
retiring generation stays leased. This lets a document keep its last working
renderer after rejecting a candidate and later receive a repaired candidate.
Escaped response bytes still count against the shared budget but grant no lease.
Staging owns its worker even if the waiter is dropped. Retirement fences
stage/publish/get, joins readers, and releases current/candidate storage; escaped
response bytes and complete leases retain their own original charges.

The explicit development option `watch = true` polls metadata of the configured
files every 250 ms. One owned worker stages and validates the complete candidate
and publishes by expected revision, retaining the original verified or paired
receipt policy. Invalid or incomplete writes retain the
current bundle and a bounded diagnostic; another observed edit retries. Changes
to bootstrap files report RestartRequired. Production defaults to no watcher.
Watch tasks and preparation readers share this activation's cleanup owner; no
filesystem work occurs on HTTP request paths.

Unchanged files are checked against their exact retained bytes in bounded chunks
and reuse the same last-reader allocation and charge across generations. This
keeps a large unchanged Worker from consuming the overlap budget twice. Changed
files still reserve their full new capacity before allocation; no digest or
metadata-only equality substitutes for byte verification.

## Product bundle admission

Receipt format 2 includes the explicit `desktop_bridge` name and version baked
into that build's JavaScript. The loader retains this descriptor with the verified
bootstrap bytes. It participates in bundle identity and cannot change during a
renderer-only publication. Desktop admission reads this retained descriptor;
it never reopens a selected package's receipt to describe an older listener.

The official application catalog and local Web adapter use
`VerifiedWebAssetsFactory`, including custom application Profiles. It validates
the bounded `rsi-build.json` receipt and exact bootstrap hashes against retained
HTTP bytes, independently of the native executable's build family. Renderer files
and ui-renderers.json retain their validated publication lifecycle and cannot
reclassify bootstrap files. Package authenticity and interface compatibility
remain with the package owner.

`PairedWebAssetsFactory` additionally requires an explicit expected family for
exact-family conformance fixtures. It is not an application catalog entry.
The ordinary `WebAssetsFactory` remains a generic library provider.
