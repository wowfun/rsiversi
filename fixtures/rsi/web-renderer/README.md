# rsi-web-renderer-fixture

This independent WASM library validates an RSI UiModel and owns actual document
nodes through web-sys. The product mount bridge imports its wrapper and complete
WASM graph from a generation lease. It owns no Worker, API client, credentials,
Profile, Session controller or Meta Runtime. Exported live-instance counts are
fixture evidence of mount/update/dispose, not a claim that browsers unload ESM.

Build with `cargo build --manifest-path fixtures/rsi/web-renderer/Cargo.toml
--target wasm32-unknown-unknown`, then run matching wasm-bindgen with `--target web
--no-typescript`. The Web product renderer fixture consumes these generated files
and checks visible Rust DOM, updates, stale host fencing and disposal in Chromium
and Firefox. The same renderer also displays a real native Service contribution:
the native source reads its Session through a restricted API grant, and the Worker
receives its model through authenticated UI API. Three open/refresh/raw-source/close
cycles verify the whole path with one Worker and zero remaining DOM instances.
