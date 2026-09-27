# rsi-web-assets

WebAssetsFactory is an ordinary native provider of immutable HttpAssets. It reads
an explicit absolute bundle directory and a closed list of flat filenames at
activation. The compiled [bundle manifest](bundle.json) owns the fixed bootstrap set, each
file's production stage, and initial renderer filenames. Rust admission, the
frontend producer and development proxy consume that same source. It is build
input, never a receipt- or renderer-supplied override. Only HTML, JavaScript, CSS, WASM, JSON and PNG
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
and publishes by expected revision. Invalid or incomplete writes retain the
current bundle and a bounded diagnostic; another observed edit retries. Changes
to bootstrap files report RestartRequired. Production defaults to no watcher.
Watch tasks and preparation readers share this activation's cleanup owner; no
filesystem work occurs on HTTP request paths.

Unchanged files are checked against their exact retained bytes in bounded chunks
and reuse the same last-reader allocation and charge across generations. This
keeps a large unchanged Worker from consuming the overlap budget twice. Changed
files still reserve their full new capacity before allocation; no digest or
metadata-only equality substitutes for byte verification.

## Product build pairing

`PairedWebAssetsFactory` requires a native build family and a bounded
`rsi-build.json` receipt inside the asset directory. At initial admission it
checks the family and exact SHA-256 hashes of the fixed bootstrap files against
the bytes retained for HTTP serving. The ordinary `WebAssetsFactory` remains a
generic library provider; the official application catalog never registers it as
an unguarded product Web entry. Custom product Profiles use the same paired owner.
Renderer files and ui-renderers.json retain their existing validated publication
lifecycle and may change independently. They cannot reclassify bootstrap files.
Pairing failures expose a consuming, bounded owner diagnostic.
