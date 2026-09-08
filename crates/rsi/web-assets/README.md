# rsi-web-assets

WebAssetsFactory is an ordinary native provider of immutable HttpAssets. It reads
an explicit absolute bundle directory and a closed list of flat filenames at
activation. The default bundle contains index.html, app.js, worker.js, styles.css,
rsi_web.js and rsi_web_bg.wasm. Only HTML, JavaScript, CSS, WASM, JSON and PNG
extensions are accepted; names contain ASCII letters, digits, dot, dash or
underscore and cannot begin with dot. At most 128 files, 128 bytes per name and
64 MiB of aggregate retained file capacity are admitted. The root document must
be index.html and is also served at `/`.

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
the filesystem or sees subsequent file edits. Reload creates a new generation.
