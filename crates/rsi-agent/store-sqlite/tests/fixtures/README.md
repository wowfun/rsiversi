# Durable writer fixtures

`persistence` holds immutable SQLite/CAS output from the production writer and
the shared Store conformance scenarios. `baseline.json` records format versions,
the normalized SQLite schema digest and exact uncompressed file digests. Gzip
retains the original bytes while keeping the repetitive corpus small. Verification works
on temporary copies and compares Header, Fact and control re-encoding byte for
byte; field order is significant for prefix digests.

Capture a new corpus explicitly with `RSI_PERSISTENCE_CAPTURE` pointing to a
nonexistent directory and run `cargo test -p rsi-agent-store-sqlite --test
persistence capture_current_writer -- --ignored --exact`. This command refuses
an existing destination. Review new fixtures as durable contract changes.
Never replace an existing corpus on a same-version change. Keep old corpora
after a version bump, where unsupported versions must be refused. These fixtures
do not claim that historical upgrades or every protocol variant are covered.
