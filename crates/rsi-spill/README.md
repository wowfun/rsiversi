# rsi-spill

Spill owns optional, best-effort full text and raw process output outside durable
Media storage. Its core defines opaque scope and locator types, bounded reads,
and nonblocking capture sinks. The local provider owns files, writer admission,
free-space checks and cleanup. The Agent policy owns text selection and previews;
neither storage nor Process knows Session scheduling or model token policy.

Locators contain a generated scope digest and object identity, never an arbitrary
path. A caller grants read access by supplying a locator through its ordinary
authenticated API. A locator is not a durable reference: expiry, cache loss or
purge of its producing scope can make it unavailable. Forks retain the same
locator without copying or changing ownership.

The standard product partitions its explicit cache root by config/data/state
authority. Each partition has one writer/cleanup lease. A Session purge fences
new captures and in-flight publications for that scope before canonical removal;
cache deletion has its own status and can be retried after canonical deletion.
Calls outside Sessions use the provider's generation scope. The product owns
this association; Process receives only a generic capture sink.

The local provider retains at most 64 MiB per complete object and reads at most
64 KiB per request. One bounded worker accepts chunks without blocking pipe
draining. Queue exhaustion, overflow, free-space refusal and I/O failure abandon
the complete object; they cannot change a command's tail or actual exit status.
`PUBLICATION_WAIT` is 250 ms. Local capture uses it for the EOF publication wait;
the Agent policy uses it as the total added wait for saving retained text,
including writes and publication. These deadlines overlap rather than add: a
backlogged save can be abandoned earlier by the policy, which keeps the original
legal Tool result. Spill availability never takes priority over that latency bound.
There is no aggregate file-count or byte quota. The default 256 MiB free-space
floor is checked on admission and in batched I/O, including pending bytes; it
cannot reserve space against unrelated writers.

Completed objects expire only when strictly older than the configured cutoff.
The default is 30 days; zero disables age expiry. Startup and hourly scans are
interleaved with writes and preserve active partials. Startup reaps abandoned
partials incrementally. Cleanup operates
only on generated children through retained directory handles. Completed files
are cache entries, with no fsync or crash-durability guarantee.

An empty capture creates no scope or object file and returns no locator. Object
creation and free-space admission start with the first nonempty chunk.

The optional Agent around-contribution uses Unicode code-point count divided by
four plus text framing to estimate inline tokens. Missing budget disables the
contribution; the standard product supplies 12,500. Only text-only results are
eligible. The retained ordered head/tail and retrieval notice must fit the budget.
Missing storage, write failure or an over-budget notice preserves the original
legal result. Notice capacity is checked before saving a new object. Canonical
program values, images and mixed content are unchanged;
`output_read` does not recursively spill. Producers must save oversized output
before constructing a bounded Tool result. A later policy cannot recover bytes
discarded upstream.

Existing locators can cover only part of a result; they do not prove that its
rendered text is recoverable. Further shortening first retains that exact text
and preserves the existing locators. Failure keeps the original legal result.
Tool results retain typed locators separately from preview text. Context pruning
preserves and renders them; compaction retains the original source coordinate so
the source result still exposes the locator. The read tool uses raw byte offsets
and reports expiry explicitly. SSH capture is excluded because its incoming
stream can already contain gaps.
