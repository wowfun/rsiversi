---
name: Configured prices frozen with Session and model intent
---

## Problem

Reported tokens cannot establish cost without an exact price and accounting
unit. Deployment/model names alone do not distinguish separately priced endpoints.

## Decision

`rsi.agent.pricing` is an optional bounded table of exact deployment, endpoint
fingerprint and model quotes. Session settings freeze at creation, including for
children; each ModelIntent copies its selected quote. Settings changes affect new
Sessions only. Rates are integer billionths of a currency unit per token. Input
rates cover inclusive input unless an explicit cache rate replaces that subset.
A differentiated rate requires its corresponding reported token subset.

The conversation reducer uses checked integer products and sums. Costs include
usage reported by failed attempts. Details disclose missing prices, missing usage
and missing required breakdowns. Currencies remain separate. An absent table
produces no cost label. Quotes are configuration, never advertised live prices.

## Alternatives considered

Fetching prices at render time rewrites history and introduces network work into
presentation. Floating point accumulation loses exactness. Guessing cache usage
or converting currencies silently creates unsupported totals.

## Consequences

Tests cover closed decode and bounded quotes/currencies, endpoint isolation, frozen settings,
per-intent price validation, failed-attempt usage, absent/partial prices, checked
overflow and exact rounding. UI and portable metrics retain explicit completeness.


The configured price can be outdated or wrong. The UI must call it configured
cost, and calculations are unavailable when usage required by a tariff is absent.

The [Session protocol contract](../../../../crates/rsi-agent/session-protocol/README.md)
owns the current durable format and validation rules.
