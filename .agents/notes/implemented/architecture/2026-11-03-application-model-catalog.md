---
name: Application model catalog and Session choices
---

## Problem

A plugin workbench needs the Host's available models and the Session's next
choice. A previous model request can include a temporary override and cannot
serve as the current selection. A separate DSH provider registry and settings
file would introduce another authority for the same requests.

## Decision

The application gateway reads the committed `LanguageModels` catalog and frozen
Agent defaults through their native contracts. The DSH Session controller maps
route identifiers and supported reasoning efforts into the released model
catalog shape. It does not install another provider registry or persist a DSH
global default alongside the native settings.

Selection writes execute the pinned native `model-selection` command. Reads use
the native Session projection at its captured Fact and control watermarks. A
failed projection remains a failure, while an absent producer uses the Header
baseline. A previous request can use an explicit override, so its request Header
is evidence for `lastUsed`, not authority for the next request's selection.

The current contract belongs to [Serve](../../../../apps/serve/README.md).

## Alternatives considered

Replaying request headers alone loses choices made after the last request.
Reconstructing the model-selection domain in JavaScript also repeats the native
codec and misses control records when reading the Fact stream. The native
projection already resolves the domain and Header baseline at a captured cut.

## Consequences
The DSH client drops projection replacements whose sequence did not increase.
Native control-only changes therefore need an actual observed event, placed
consistently beside conversation Facts. Store retains the committing append's
final Fact watermark for every control. Facts precede controls in that atomic
append, and controls sharing a watermark retain their own order. The bridge can
project a committed choice into a DSH model-selection event at that position.
This keeps the native control/context separation while providing a stable merged
order. Sorting timestamps would fail under clock rollback, and a bridge-local
counter would lose the order at restart. The Store schema changes explicitly;
earlier databases remain unsupported rather than receiving guessed anchors.
