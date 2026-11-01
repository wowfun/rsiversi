---
name: Resolve prepared output defaults without rewriting retained requests
---

## Problem

The Language router inserted a declared output default with a request builder.
Retained requests correctly refuse mutation because their admission guard accounts
for the exact immutable allocation, so valid defaulted calls failed preparation.

## Decision

The [router contract](../../../../crates/rsi-ai/core/README.md) keeps the caller's
request and digest intact. The frozen model profile owns the default; concrete
adapters resolve the effective scalar through the provider context. Portable
providers receive both the original request and frozen profile.

## Alternatives considered

Removing the retention guard permits unaccounted mutation. Copying the request
and reattaching its old guard silently misstates allocation ownership. A second
full prepared-request representation duplicates the semantic request for one
bounded scalar default.

## Consequences

Provider authors resolve explicit-or-profile output limits at translation. The
snapshot digest identifies the original request; the captured profile separately
identifies effective defaults. Direct and deferred calls use the same rule.
