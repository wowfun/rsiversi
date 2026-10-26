# rsi-parser-fuzz

This isolated cargo-fuzz workspace exercises the production apply-patch grammar
and MCP JSON-RPC/SSE framing through feature-gated, parser-only seams. Inputs are
bounded by the production limits; no filesystem mutation, credentials, provider
or network is involved. The apply-patch engine requires Linux.

On Linux with nightly and cargo-fuzz installed, run from this directory:

```sh
mkdir -p generated-corpus/apply_patch generated-corpus/mcp_frame
cargo +nightly fuzz run --fuzz-dir . apply_patch generated-corpus/apply_patch corpus/apply_patch -- -max_total_time=60 -max_len=2000001
cargo +nightly fuzz run --fuzz-dir . mcp_frame generated-corpus/mcp_frame corpus/mcp_frame -- -max_total_time=60 -max_len=2097155
```

The checked-in UTF-8 patch, traversal rejection, JSON-RPC and split SSE seeds
also run as deterministic tests in the owning crates with `--features test-support`.
Fuzzing is opt-in evidence, not a default gate or proof of parser completeness.
The weekly/manual performance workflow compiles both fuzz targets with their
locked fixture manifest on Linux; that check does not run a fuzz campaign.
Shared dependencies resolve to versions in the product Cargo.lock; harness-only
dependencies remain fixture-owned. Run `python3 fixtures/rsi/parser-fuzz/check-lock.py`
from the repository root after either lockfile changes.
MCP seeds begin with a two-byte little-endian chunk selector followed by raw wire bytes.
The core crates keep their raw parser seeds in their own `tests/corpus` directories.
The first corpus directory receives generated cases; checked-in seeds remain read-only inputs.
Promote minimized discoveries deliberately into owning deterministic tests.
The deterministic corpus CI step lints the selected parser targets with
`--no-deps`; each dependency's owning job retains its full lint policy.
