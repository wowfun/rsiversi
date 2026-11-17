# Writer provenance

Captured on native Windows from the working-tree production navigation writer,
based on repository commit `af4b8b395aca746f2204eb211fab2aa1a6e2bcd0` plus the
approved typed-Domain implementation. This is the first domain-2 baseline, not a
historical release artifact.

`tests::persistence::capture_current_writer` invokes `Navigation::write_metadata`
and order publication through the real Domain queue. The memory backend records
the raw `EncodedValue` payloads actually committed by that writer. The capture
covers title, pin, archive, removal and account ordering; it contains no user
state or credentials. `baseline.json` binds the version, schema and exact payload
file digests. The same owner test reopens these records through Domain and checks
codec bytes and schema, while the retained domain-1 corpus is explicitly refused.

The capture used the navigation library test executable produced by the grouped
consumer behavior build; `.local/audit/gap-closure-20261116/` retains its capture
and replay logs. Capture is opt-in and requires a new destination directory.
