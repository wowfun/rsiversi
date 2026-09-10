---
name: One product bootstrap for native role catalogs
---

## Problem

Service-owned staging starts after the Application catalog is frozen. That
ordering cannot supply Application or remote Client factories, and deriving
Agent presets before the Service owns its Settings loses configured roots and
default selection.

## Decision

A stable product bootstrap owns native staging before Application, Client and
Service child Profiles. It shares one manager and Loader across those roles,
with immutable catalogs submitted through ordinary Profile input replacement.
Agent sources combine that manager's exact staged factories with the Service's
own Settings-backed preset catalog. They neither discover nor load code while
selecting an Agent generation.

Native manifest/index format 2 declares scope. The explicit absolute `source_root`, or the manifest directory when omitted,
owns relative artifacts, build cwd and bounded watch inputs; both source and
manifest directory identities are pinned. Full linked identity preflight uses
the actual assembled Service, Application and domain Client catalogs, including
their transport plugins, before native loader admission.

An Application bootstrap uses the published catalog to keep linked inspection
available after failed staging. Fresh Agent selection and subsequent candidate
capture still fail on failed or pending staging. Remote applications do not
acquire a local Service Owner or materialize backend presets. Their independent
native cache slot remains fixed for the entire bootstrap lifetime. A bounded
pool of 64 slots reuses the Loader's exclusive lifetime lock, so sequential
launches reuse directories while simultaneous or failure-retained owners stay
isolated. Allocation skips only locked slots; malformed paths fail closed.

## Alternatives considered

A Loader per role or a rotating live cache bypasses failed-finalization
accounting. A mutable shared registry breaks frozen generation identity. A
bootstrap-wide default preset discards the Service's configured selection.
Implicit manifest migration adds an unnecessary pre-release state mutation.

## Consequences

An acquired Service Owner remains retained until native cleanup completes;
failed finalization keeps it and executable mappings until process exit. Real
native A/B tests exercise all four roles, rollback, cleanup and remote startup.
Existing command-line tests cover configured defaults, failed-stage inspection
and absence of remote backend state. Source tests cover directory replacement,
quotas, explicit builds and cancellation.

Independent embedded Services with distinct HostPaths may still share a Runtime
while retaining separate fixed owner/cache identities. This is an existing public
embedding contract; one Loader per product bootstrap is not a Runtime-global
registry. Linked entry points and Worker code retain restart requirements.
