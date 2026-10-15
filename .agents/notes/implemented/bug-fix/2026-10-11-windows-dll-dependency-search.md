---
name: Bounded Windows native DLL dependency search
---

## Problem

The native loader pins and verifies the selected DLL, but `Library::new` uses
Windows' default dependency search. An unrelated working directory or PATH can
therefore select executable dependency bytes outside the staging authority.

## Decision

The [native loader](../../../../crates/rsi-meta/native-loader/README.md) uses
`LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32` on Windows.
Unix mapping retains its existing behavior. Mapping errors propagate through
the existing load result, without retrying a broader search.

## Alternatives considered

Changing the process-wide DLL directory would affect unrelated concurrent
loads and introduce ambient authority. Keeping the default search would leave
the selected dependency dependent on process state. Automatically copying or
installing dependencies would require a separate provenance and publication
contract that the current artifact API does not provide.

## Consequences

The selected DLL's own directory and SYSTEM32 are dependency authority. The
catalog still owns staging and pins the primary bytes; this change does not
hash or independently authenticate dependencies. A missing runtime refuses
mapping. MSVC/UCRT deployment must satisfy the SYSTEM32 prerequisite. Real
Windows library-boundary tests verify adjacent dependency loading and refusal
of dependencies available only through the working directory or PATH.
