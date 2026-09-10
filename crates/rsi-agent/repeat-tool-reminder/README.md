# rsi-agent-repeat-tool-reminder

An ordinary Agent PostTool contribution detects consecutive exact calls and
adds source-attributed advice after their unchanged settled results. Configuration
is null or a closed object containing `thresholds` (default `[3, 5, 8]`), optional
`include` exact Tool names, and `exclude` exact Tool names. Thresholds contain
1–32 unique integers in 2–1,000,000; each name list contains at most 64 unique
Tool-protocol names. Invalid configuration fails before activation. Excluded
calls neither increment nor reset the tracked chain.

Identity covers the complete Tool name and JSON arguments. Recursive object key
order is ignored; array order and exact arbitrary-precision numeric representation
are preserved. A streaming SHA-256 encoder sorts borrowed object fields and
retains no second encoded arguments buffer or argument preview. ToolResult's
canonical result/provenance is never rewritten to carry reminder metadata.

The version-1 `rsi.repeat-tool-reminder` domain retains a Session/Turn-bound Fact
cursor and one bounded signature/name/count chain. A new Session or Turn resets
the chain. A newly entered Human message resets it within the Turn; Agent,
completion and plugin inputs do not impersonate Human input. Forked state resets
on the child's distinct identity. Recovery and history reads never rerun a
callback; a new Turn after restart starts a new chain. Reobserving an already
processed settled batch produces no duplicate advice or state mutation.

Callbacks consume the supplied source-ordered batch, bounded by the AI output's
256-block limit. They read one bounded captured Fact page at a time from the
retained cursor through the exact horizon and correlate each settled ToolResult
to its ToolIntent using both EffectId and full ToolResultIdentity. Only the
current batch's bounded name/digest tuples are retained. Missing or inconsistent
intents fail before proposing changes. Cursor/state and any advice are committed
together by the framework. The framework's deadline/cancellation bound the scan.

DSH informed exact matching, thresholds, per-agent separation, transparent
excluded calls and advice without Tool-result rewriting. RSI binds the heuristic
to a durable Turn cursor for bounded incremental reads and atomic advice. RSI's
current policy/approval refusals produce ToolRejected and terminate that Turn,
so its driver does not deliver those refusals to the ordinary post-tool batch.
Settled ToolResult errors, including nonzero process outcomes, still count. The
callback can interpret a supplied ToolRejected's own complete call identity, but
this does not imply that the current executor continues after a refusal.
