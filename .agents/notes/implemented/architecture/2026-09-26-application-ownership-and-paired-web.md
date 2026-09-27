---
name: Application ownership and paired Web publication
---

## Problem

The source tree groups document assets and presets under plugins while concrete
applications are selected inside the reusable core. Stale assets beside a newly
built executable can reach Profile activation before incompatibility is visible.
The Service also needs application metadata for Profile management and native
plugin reservations, so moving only launchers cannot establish dependency direction.

## Decision

Root-level apps owns the standard product's entrypoints and explicit application
catalog. Core consumes frozen application metadata and a catalog provider, while
retaining Service lifecycle and native staging. Clients and daemons share metadata.
Presets live with the library that embeds their bytes. Product Web publication
pairs native executables, immutable bootstrap resources and the Worker; independently
published renderer generations retain their existing lifecycle. Supervised Vite
source development is an explicit document-only exception, with paired upstream
native/Worker assets. Build tooling belongs to apps; repository verification stays
in xtask.

## Alternatives considered

Keeping application factories in core would retain reverse dependencies. A CLI
library shared by Desktop would misname the shared catalog's responsibility.
Checking only the local Web launcher would let custom Profiles bypass validation.
Pinning the initial renderer digest would break supported live renderer publication.
Requiring immutable document source in development would remove the chosen Vite
workflow. Stable serialized source capture and shared build caches avoid cold
compilation at a new source path for every publication.

## Consequences

Applications consume libraries without reverse normal, build or test dependencies;
a deterministic architecture gate checks workspace and maintained standalone packages.
Native clients and daemon Profile management use the same metadata. Product assets
are admitted once from the bytes subsequently served, before local launch effects.
Worker initialization rejects another build family before Profile bootstrap.
Renderer generations retain independent digest validation and publication.

Initial paired builds compile native and WASM targets and are expensive. Stable
source capture and shared caches reduce subsequent builds. Active publication pins
prevent reclamation until their processes exit; retention limits apply only to
inactive generations. Explicit external publications remain caller-owned. Mutable
Vite document source is deliberately outside the release pairing guarantee;
Worker/WASM and native processes still come from the paired upstream.
