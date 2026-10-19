---
name: Session-owned evidence section retention
---

## Problem

Paging a large immutable request section otherwise materializes its whole Fact
and checks its reference on every small page. A general decoded-Fact cache would
also retain unrelated model logs and broaden ownership beyond the requested data.

## Decision

The Session service generation retains only a selected section and its exact
binding under the [Session contract](../../../../crates/rsi/session/README.md).
Each request still passes current Session admission. Equal cold requests share
one service-owned load; reservations and activity follow actual Store completion,
including after cancellation. Admission plans all necessary idle evictions before
changing retained state, so rejection preserves warm entries. Selected text moves
out of the owned Fact instead of being copied from a borrow.

## Alternatives considered

Uncached reads avoid retention but repeat expensive materialization for every
page. A full-Fact cache could improve other readers but needs a broader owner and
budget without evidence that those readers benefit. Releasing reservations on
caller cancellation would allow still-running Store work to exceed admission.

## Consequences

Worst-case reservations can evict small idle sections before their replacements'
actual sizes are known. Pinned sections can reject a new cold load. The selected
text budget does not cover transient Store materialization; Store retains its
own bounds. Shutdown reports incomplete cleanup if Store work does not settle
within its deadline, while that work keeps its reservation until actual exit.
