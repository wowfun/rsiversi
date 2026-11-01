---
name: Workflow continuation authority uses each domain owner's codec
---

## Problem

Kernel intersects exact domain identities when a non-human Turn creates a
workflow. A duplicated Goal codec literal admitted no automatic Goal workflow
after the Goal owner advanced its codec, even though ordinary human work still
succeeded. Schedule had the same source of future drift.

## Decision

Goal and Schedule export their codec versions. Their domain registrations and
the workflow allowlist consume those values. Kernel retains exact identity
matching. Tests issue real continuation leases, reserve and claim automatic
Turns, then exercise workflow preparation, durable acceptance and completion.
Absent or wrong-codec allowlists are refused; the owner's exact identity works.
The [Program contract](../../../../crates/rsi-agent/program/README.md) owns current behavior.

## Alternatives considered

Matching only the domain name would silently authorize an unreviewed codec.
Independent literals invite drift. A decode-only test checks constants but
cannot establish that non-human creator authority actually crosses Kernel.

## Consequences

Codec changes remain explicit durable-contract changes. An automatic round
still needs its live continuation and exact started model-origin Workflow Tool;
the shared constant grants neither authority by itself.
