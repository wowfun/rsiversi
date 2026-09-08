# rsi-api-protocol

A completed ByteReceiver can transfer its allocation into another explicit byte
budget. It compacts unused capacity and acquires the destination lease before
releasing the source lease; rejection releases the unfinished transfer. Immutable
clones and slices then retain the destination lease until their last owner drops.

LocalCompatibilityKey is an opaque 256-bit deployment-selection fence for native
local transports. The caller owns its derivation and build/launch policy; the
foundation validates and compares canonical bytes. It is not authentication.
Native peer credentials establish local authority, independently of this key,
EndpointId and HostEpoch. Remote device authentication never grants local origin.

This library owns validated API operation identities and immutable, accounted
transport buffers. It contains no platform runtime, network stack, authentication
store or domain implementation.

A byte budget rejects excess retention before copying or encoding. A reservation
may be transferred to immutable bytes, which retain that charge through clones
and slices until the last owner drops. Encoding measures the same serde value
before reserving and enforces the admitted bound again during serialization.
Pure JSON measurement stops at its supplied maximum without allocating an encoded
payload, so domain validators can enforce their own smaller limits before copying.
A buffer from an owner cannot release its charge while a slice still references
that allocation. Separate budgets express input, output and transport scratch
ownership; a single buffer is charged only to its owning budget.
Finite response capacity distinguishes already reserved storage from measured
read delivery. Splitting binary parts reduces the remaining whole-response
ceiling; each part acquires its shared byte charge before copying. Transport
receivers explicitly reserve the ceiling before receiving unknown-length data.
Receivers may compact a completed allocation before transferring it: the maximum
reservation remains held while unused storage is released, then shrinks to the
remaining capacity. Retained slices never release capacity still allocated.
For payloads whose length is not declared, ByteAccumulator admits storage as
fragments arrive, before copying or growing its allocation. Each accumulator has
an operation maximum; all partial accumulators share their receiving budget.
Growth may reserve spare capacity within those bounds and falls back to the exact
required capacity when the shared pool cannot admit that spare space. Completion
compacts storage in its receiving budget or transfers it to an explicit retention budget. These bounds
account storage capacity, not allocator-internal reallocation peaks.

Operation identity is a validated domain/name pair and positive version. It does
not encode an executable build hash or a running Host epoch. Those are separate
connection policies. The registered operation determines its control/data/
subscription class and whether an admitted call survives waiter cancellation.

A domain registers an exact operation descriptor and handler through the write-only
registrar contract. The dispatcher opens one admitted invocation from that
registered descriptor and a trusted caller origin. The invocation exposes its
request budget and transfers its input owner into execution. Calling `invoke`
on a mutation schedules owned work before returning a waiter; dropping that
waiter cannot undo admission. Read invocations and returned streams are cancelled
by retirement or caller drop. A registration closes new admission immediately
and can await the drain of work already owned by it.

Infrastructure failure after mutation dispatch cannot prove non-execution. Panic,
invalid response shape, failed result encoding or a lost result is an unknown
outcome, reconciled through the domain's own identity and status operation. A
typed domain error may establish a stronger result. RSI client implementations never automatically
retry an unknown mutation or invents a durable request-outcome database.

EndpointId names a persisted deployment across restarts and code upgrades.
HostEpoch names one running Host generation. DeviceId names one authenticated
device. Their equal-sized representations are distinct Rust types, validated on
decode; they are not interchangeable with an operation version or local launch
key. The shared protocol can construct identities from explicitly supplied entropy;
native callers may use OS generation without making browser parsing depend on it.

`ApiClientContract` exposes one negotiated connection generation, its bounded
operation catalog and raw retained-byte calls. Domain proxies require their exact
operation descriptor before sending input. Client retirement cancels observations
and rejects new calls; it never grants authority to stop the remote deployment.
The connection description and catalog DTOs are shared between transports and
clients. Catalog decoding rejects duplicates and more than 2,048 entries.

The shared stream handoff has one item slot and a separate terminal signal.
Its owner drives the producer independently of consumer polling, reserving the
slot before polling another source item. Retirement releases even an unpolled
producer; a lost driver or source failure cannot appear as clean EOF.

The typed finite JSON helper decodes the domain's request type before calling its
handler and encodes either its reply or its closed domain error under the admitted
response reservation. Encoding failure after handler entry remains infrastructure
failure, so a dispatched mutation is reported as unknown. Typed client decoding
applies the same rule to malformed successful or domain-error responses.
