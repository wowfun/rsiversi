---
name: Repeat advice as an ordinary Agent contribution
---

## Problem

Repeated identical calls can waste a coding Turn. The executor already owns
settled-result ordering and immutable context publication; adding a special
heuristic there would mix business policy with effect scheduling.

## Decision

The standard preset selects an ordinary domain and PostTool contribution. The
[owning contract](../../../../crates/rsi-agent/repeat-tool-reminder/README.md)
defines complete argument identity, transparent filters and thresholds. A
Session/Turn-bound cursor advances atomically with any advice. Its bounded scan
correlates results to existing intents without duplicating arguments in Facts.

DSH informs exact matching and additional advice. RSI retains its existing
terminal policy-refusal behavior; those refusals do not reach the driver's
normal PostTool batch. New Turns and forked Session identities reset the chain.

## Alternatives considered

A process-local weak map would discard state independently of its durable
advice and complicate generation replacement. Copying arguments into results
would expand the canonical format solely for one heuristic. Rescanning whole
history after each batch would grow work without bound. Continuing after policy
refusal would change executor behavior beyond this contribution's scope.

## Consequences

The plugin retains one bounded signature chain and scans only newly captured
Facts. Domain codecs remain required for cold execution under current presets.
Streaming hashes avoid an encoded argument copy; borrowed object-key sorting
still costs memory proportional to the bounded argument object's field count.
