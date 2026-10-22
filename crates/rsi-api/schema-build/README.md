# rsi-api-schema-build

This build library generates conservative wire-contract snapshots from the
owning crate and its path dependencies. It records Serde models, attributes,
custom serializers/deserializers, aliases and model-generating macros as
normalized Rust syntax, independent of whitespace, ordinary comments, build
profile, compiler target and executable bytes. An explicit `wire-contract.toml`
owns the protocol name and semantic model version. The digest includes the
repository's exact JSON number and object-order policy.
An optional `sources` list names additional codec/framing sources relative to
the owning crate, including JavaScript/TypeScript peers. Entries must resolve
inside the workspace. Their complete UTF-8 bytes, with normalized line endings,
enter the snapshot conservatively; they are not parsed as Rust or treated as
proof of semantic equivalence.
Binary codec owners also contribute their struct/enum definitions, including
implementations split across source files. Explicit integer `repr` enums retain
their discriminants. Unrelated presentation structs do not enter a contract
merely because they share a source file with a codec.
Model constructors, validation/conversion methods and transitively referenced
free helpers also contribute to admission semantics. Dependency feature
declarations are recorded per normal or target-specific edge, including
workspace inheritance; Cargo's package feature definitions are retained.
The snapshot also records each normal external dependency's resolved version,
source and checksum from Cargo.lock, selected through its owning package's
lockfile edges. This covers externally supplied wire models and codecs without
hashing unrelated workspace packages or executable artifacts. Target-specific
dependencies are included on every build target.

This is a Rust wire-contract schema, not a JSON Schema validator. Capturing
custom codecs matters because JSON Schema alone cannot describe their wire
behavior. The digest is a conservative source admission fingerprint, not proof
of wire incompatibility. Dependency implementations and target-only models can
change admission across protocols, even for a private helper edit. A narrower
boundary requires explicitly audited model dependencies; dropping Cargo edges
without that evidence could admit incompatible clients.
The snapshot is intentionally conservative: a custom codec change
requires review even when it might preserve encoded output. Semantic invariants
still require owner tests and an explicit version change.

Build generation reads source; it executes no dependent package. Runtime
consumers compare only required protocol descriptors. Repository verification
compares committed snapshots and requires a version increase when a snapshot
changes relative to the selected Git base.

A verification invocation reuses parsed package fragments across contracts. The
cache is scoped to that immutable source observation, never shared across watch
iterations. Build scripts emit descriptors and input dependencies only; reviewed
snapshots are written exclusively by the repository verification command.

Format 2 excludes test functions and test-only source directories. Explicit Rust
source inputs use normalized syntax with test items and documentation removed.
The committed snapshot contains the exact pretty-printed UTF-8 bytes being hashed,
without a trailing newline. Native builds reject stale snapshots before emitting
a descriptor; review and regenerate through `cargo xtask verify-protocols --write`.
JavaScript hashes those same bytes and never reserializes them.
