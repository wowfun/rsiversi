---
name: Admit WebKitGTK custom-scheme requests by their exact document Referer
---

## Problem

Requiring Origin on every private native POST rejects normal Linux WebKitGTK
requests. A minimal probe using the same secure custom-scheme registration as Wry
on WebKitGTK 2.52.6 observes no Origin and the exact `rsi://localhost/` Referer.
Product Desktop CI consequently fails before its first conversation.

## Decision

The [Desktop contract](../../../../apps/desktop/README.md) admits the Linux-only
missing-Origin case through exactly one Referer naming the application root.
Main-window, normalized callback URI and POST admission remain required. A supplied
Origin must still be exact and unique. Preview documents and missing authority
headers do not enter this exception. Other platforms retain Origin admission.

## Alternatives considered

Accepting any absent Origin restores operation without identifying the document.
Client-set tokens or custom headers would create another authority distribution
mechanism. The native browser already supplies the exact document Referer.

## Consequences

Boundary tests cover the actual WebKitGTK header shape and foreign, preview,
duplicate and absent header refusals. Paired Desktop CI exercises product runtime
behavior; the minimal probe alone does not establish all Desktop functionality.
