# rsi-api-contract-build

Each protocol owner declares `{ "name": "rsi.example", "version": 1 }` in
`wire-contract.json`. Names start with a lowercase ASCII letter and contain at most
128 lowercase letters, digits, dots, underscores or hyphens. Versions are positive
u32 values. The closed declaration is at most 1 KiB.

Build scripts call `build()` to emit `RSI_WIRE_NAME` and `RSI_WIRE_VERSION`.
Only the declaration is a build input. Source files, dependency versions, platform
code and compiler versions do not participate in protocol identity. Protocol owners
version incompatible wire semantics and maintain actual codec conformance tests.

`cargo xtask verify-protocols` checks all declarations and unique protocol ownership;
it neither rewrites files nor compares against Git history.
