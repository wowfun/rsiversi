# rsi-parser-fuzz

This isolated cargo-fuzz workspace exercises the production apply-patch grammar
and MCP JSON-RPC/SSE framing through feature-gated, parser-only seams. Inputs are
bounded campaign inputs include over-limit cases (the MCP cap admits two maximum
frames plus framing bytes); the production parser keeps its own admission limits.
No filesystem mutation, credentials, provider
or network is involved. The apply-patch engine requires Linux.

On Linux with nightly and cargo-fuzz installed, run from this directory:

```sh
mkdir -p generated-corpus/apply_patch generated-corpus/mcp_frame
cargo +nightly fuzz run --fuzz-dir . apply_patch generated-corpus/apply_patch corpus/apply_patch -- -max_total_time=60 -max_len=2000001
cargo +nightly fuzz run --fuzz-dir . mcp_frame generated-corpus/mcp_frame corpus/mcp_frame -- -max_total_time=60 -max_len=2097155
```

The checked-in UTF-8 patch, traversal rejection, JSON-RPC and split SSE seeds
also run as deterministic tests in the owning crates with `--features test-support`.
Fuzzing is weekly/manual evidence, not an ordinary pull-request gate or proof of
parser completeness. The performance workflow checks the locked fixture manifest
and runs each target for 60 seconds with nightly-2026-10-24 and cargo-fuzz 0.13.2.
The first corpus directory is fresh runner-temporary storage; the second is the
checked-in seed directory. Generated corpus is neither uploaded nor cached.
Each single-process target writes failure input to its own fixed artifact path.
Only a failed target's bounded artifact and command/toolchain/commit provenance
are retained. Checkout dirtiness includes tracked changes and non-ignored untracked
files, including local harness code and corpus seeds. Ignored build outputs are not
source provenance. Temporary corpus has a job lifetime, not a claimed byte quota.
The input limits above also bound retained failure inputs. Oversized or empty
checked-in seed collections fail before launching the campaign.
`python3 fixtures/rsi/parser-fuzz/campaign.py TARGET NEW_OUTPUT_DIRECTORY` runs
the same campaign locally; output and generated corpus remain outside Cargo caches.
The runner verifies that cargo-fuzz did not change the fixture lockfile and
preserves a failed command's status. Its tests use temporary Git repositories and
fake fuzz subprocesses; they never run fuzz campaigns.
Shared dependencies resolve to versions in the product Cargo.lock; harness-only
dependencies remain fixture-owned. Run `python3 fixtures/rsi/parser-fuzz/check-lock.py`
from the repository root after either lockfile changes.
MCP seeds begin with a two-byte little-endian chunk selector followed by raw wire bytes.
The core crates keep their raw parser seeds in their own `tests/corpus` directories.
The first corpus directory receives generated cases; checked-in seeds remain read-only inputs.
Promote minimized discoveries deliberately into owning deterministic tests.
The deterministic corpus CI step lints the selected parser targets with
`--no-deps`; each dependency's owning job retains its full lint policy.
