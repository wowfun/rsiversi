---
name: rsi-ai native plugin conformance
comment: Package-local staging and loader validation for the provider workspace
---

## Problem

The `rsi-ai` provider plugins build in one standalone Cargo workspace, while
`rsi-meta` resolves every native artifact relative to its package manifest.
A build-only gate can therefore pass even when the published package path is
unsafe or missing. The shared provider wrapper also depends on runtime ticks to
resume bounded output after host backpressure, so omitting that injection can
deadlock an otherwise valid build.

## Decision

`cargo xtask rsi-ai conformance` owns the assembled native check. It fetches the
locked plugin workspace, runs offline format, lint, and tests, builds every
native provider, copies each host artifact into that package's
`target/native/<target>/` directory, and asks `rsi-meta-loader` to open and
validate every manifest against the current host ABI and target. Each provider
manifest declares the required `runtime.tick` injection and contains only
package-relative artifact paths.

The checked-in manifests name Linux x86_64 and macOS arm64 artifacts. The local
gate executes only the current supported host target; CI supplies the native
platform matrix instead of cross-running dynamic libraries.

## Alternatives considered

Pointing package manifests at the shared workspace target directory was
rejected because it requires a parent traversal that the loader deliberately
forbids. Giving each package a separate Cargo target directory was rejected
because it repeats the expensive provider build. Treating manifest parsing as
documentation validation was rejected because the actual loader owns path,
target, and ABI policy.

## Consequences

Conformance performs a small deterministic copy after the shared release build
and leaves package-local staged artifacts ignored by source control. A passing
gate now proves both compilation and loader selection for every provider on the
executed host. Runtime-tick delivery is part of the manifest contract, so
backpressure progress no longer depends on an undeclared host service.
