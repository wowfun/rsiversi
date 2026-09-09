# rsi-ai-portable

An ordinary provider plugin imports one explicitly configured `rsi.ai.portable`
service through the [protocol](../protocol/README.md). It publishes enabled
Language and Image facets atomically through the existing provider registrars.
Cached, validated Describe declarations drive synchronous compatibility checks
before credential or media resolution. It creates no registry or runtime.

Configuration selects service, deployment, provider family, protocol, endpoint
fingerprint, optional credential reference, retry policy, and enabled facets.
The provider Fiber generation and exact injected Portable capability freeze the
binding. Configuration accepts no inline credential. Descriptions and Prepare
replies are bounded to 256 KiB. Prepare preserves the caller's snapshot and
retains only the frozen typed input and bounded transient state. It sends no
credential or media bytes. The consuming Start opens exactly one attempt;
there is no adapter retry or deferred-operation implementation.

Start permits at most 2048 dependency requests and 256 MiB of dependency reply
bytes per attempt. Each reply is at most one binary packet; Media pages must
belong to an exact descriptor in the frozen request and lie within its declared
body. The original PrepareContext retains atomic media admission, digest
verification, and caching. Credentials are sent only as binary packets. Transport
copies owned by Meta or native code do not promise zeroization. Native code has
process authority; this protocol is not a sandbox or an attestation of provider
behavior.

A process-wide 64 MiB ByteBudget bounds retained bridge JSON and receive packets.
Meta separately accounts queued fragment Messages. Typed decoded values and
consumer-owned Image chunks have their own semantic limits; the wire budget is
not an RSS ceiling. Image bytes never enter JSON. Native failure diagnostics are
replaced with static bridge summaries. After sending Start, dispatch is Unknown
unless a native typed failure explicitly supplies evidence; malformed exchanges
cannot turn uncertain effects into retryable NotDispatched facts.

Every semantic terminal is held until a clean Portable terminal is observed.
Callers still validate stream grammar with the existing assemblers. Error paths
cancel and drain the Meta driver; dropping a stream cancels via CapabilityCall.
A non-cooperative native callback can remain retained under the Loader contract;
bridge cancellation is not proof of foreign quiescence. Provider withdrawal
uses the existing generation gate. Global provider changes require drain/restart;
this package does not hot-swap same-name routes. Retirement fences even a
previously prepared Portable Start. A completed cancellation can precede foreign
callback exit, so an immediate new call may encounter the Loader's busy or
reentrant gate. The adapter reports that failure without retry; pure fixture
Prepare probes establish eventual reuse independently from call cancellation.

Tests use keyless public Local routers and actual Portable channels, including a
real native fixture. The default DeepSeek deployment remains independently owned
by its provider package and uses OpenAI Responses.
