---
name: Bounded request evidence inside ModelIntent
---

## Problem

Prepared-call digests identify an actual request but cannot explain its system
instructions, tools or settings. Rebuilding context during inspection can produce
a different request and can execute contributors a second time.

## Decision

Executor extracts evidence from the single LanguageRequest passed to Prepare,
then pairs it with the returned generation-pinned snapshot. ModelIntent contains
the complete optional evidence package atomically. Configuration, system and tool
sections are inline or reference an earlier inline section in the same Session.
References bind sequence, section, SHA-256 and byte length and never chain. The
Store protocol owns original lookup and binding for both admission and inspection.
Kernel retains a bounded cache of validated original descriptors across attempts;
recovery uses the same mechanism for unfinished work. Neither cache retains text,
and eviction only causes another validated read. A 64-entry discardable cache retains only
deduplication metadata. Ordinary messages and media appear only in a typed count
and byte manifest; credentials, raw HTTP and media bodies are never copied.

Decoded evidence is at most 16 MiB per request. Kernel independently accounts for
at most 16 MiB new inline bytes per Turn, recovered from durable intents and also
charged to the existing generated-record budget. Only an explicit optional
evidence budget rejection before publication permits replacing the whole package
with Unavailable on the same prepared effect. A successful flush of the preceding
prefix leaves that batch unpublished and eligible. I/O errors, failed flushes,
uncertain publication and stale claims do not permit a fallback attempt. Inspection pages are 256 KiB.

## Alternatives considered

A second CAS store creates independent orphan collection and publication rules.
Re-running context contributors is not evidence of the dispatched request.
Referencing references makes reads and validation grow with Session age.

## Consequences

Tests cover once-built request capture, inline/reference identity, forbidden chaining,
cross-Session rejection, budget/recovery accounting, safe prepublication fallback,
bounded cache and page reads, no duplicate ordinary text or credential payloads.


System/tool evidence can contain user-controlled instructions and must be treated
as inspectable data. Optional evidence may be unavailable without changing the
underlying model operation. Cache eviction can cause another bounded inline copy.

The [Session protocol contract](../../../../crates/rsi-agent/session-protocol/README.md)
owns the current durable format and validation rules.
