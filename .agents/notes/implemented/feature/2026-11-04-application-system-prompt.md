---
name: Capture application system guidance with request Tools
---

## Problem

The standard native executor captured application Tools but ignored system
sections registered in their owning Cordis tree. A plugin's model instructions
therefore did not accompany its Tool definitions. DSH assembles its prompt after
model and Tool selection, before pre-step message decisions.

## Decision

Assemble original application system sections in the existing request capture,
using its owned Agent, exact model selection and resolved Tool set. Retain the
rendered text with the request pin and publish it through ordinary Step
instructions under a framework-assigned producer identity. Keep the combined
instruction limit at the native admission and durable boundaries.

## Alternatives considered

A separate prompt RPC adds a round trip and permits a different model or registry
observation. Rebuilding a preset for each dynamic section leaves resident Sessions
on stale generations. Appending these sections as user messages gives them the
wrong role and accumulates obsolete guidance in history.

## Consequences

Retries retain the same prompt and catalog; later captures can replace or remove
guidance. Current preset system guidance still composes with the application's
sections. The application Tool protocol is version 5; durable formats are unchanged.
Original DSH rendering, interpolation, ordering and scoped assembly remain the
text authority. Capture shares the existing BeforeStep stage deadline instead of
the former three-second discovery cutoff or ten-second receipt transport timeout.
An unavailable process still returns immediately; asynchronous assembly has the
same stage budget as other preparation callbacks. This does not implement dynamic
context snapshots or the pre-step
message waterfall. Their rewrite/drop behavior needs a separate durable distinction
between claimed input and final model input.
