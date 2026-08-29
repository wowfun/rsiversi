# Developing rsi-meta

Use the pinned Rust toolchain. Change the owning public contract before implementation, keep core free of artifact and product policy, and add the closest behavior test for every lifecycle or routing change.

The standard product gate is:

```sh
cargo xtask rsi-meta conformance
cargo xtask verify-docs
RUSTDOCFLAGS="-D warnings" cargo doc --locked -p rsi-meta-contract -p rsi-meta -p rsi-meta-scope -p rsi-meta-profile -p rsi-meta-native -p rsi-meta-native-loader --no-deps
```

Native failures must be reproduced through an actual dynamic library when the
host platform supports it. Do not refresh a baseline or weaken a bound merely
to make a gate pass. An evidence-backed reviewed baseline adjustment is valid
when the measured file still owns one cohesive contract; avoid mechanical
splits whose only purpose is preserving an old line count.
