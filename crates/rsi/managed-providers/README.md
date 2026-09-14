# rsi-managed-providers

Model discovery holds the configuration grant and a separate two-request
admission limit. It checks the caller's configuration authority before validating
endpoint semantics. Each request freezes endpoint and credential reference,
resolves the secret at the Host and performs one cancellable HTTP GET with redirects
disabled and a 30-second whole-operation deadline. Concrete providers own URL
and response translation; candidates are bounded to 4,096 within 4 MiB.
Discovery returns no raw response body or secret and changes no durable state.
Its configuration lease lasts until the request completes or is cancelled.
The lease shares one of eight configuration slots and a revocation drain fence;
it is not an exclusive writer lock. Discovery can occupy at most two slots.

A configuration grant is Host-operator authority to select provider destinations
and send a selected provider-owned credential there, including custom HTTP(S),
loopback and private-network endpoints. Discovery has the same destination trust
as managed inference configuration. It is not a restricted network-fetch grant:
the Host imposes no private-address blocklist or HTTPS-only policy. Operators
must grant it only to principals trusted with those credentials and destinations;
HTTP sends the bearer without transport encryption. Session/model read authority
does not confer this grant. Redirects remain disabled.
The declared ConfigurationAccess dependency follows Meta retirement ordering;
API registration retirement cancels discovery reads before draining them, and
owner cleanup cancels direct discovery calls before closing the child Profile.
Closed discovery admission reports `ShuttingDown`; only occupied open admission
reports `Capacity`. Provider rate limiting also reports `Capacity`; connection,
deadline, server and malformed-response failures report `Backend` with safe
diagnostics. Invalid requests and provider credential rejection report `Invalid`.
Shutdown remains `ShuttingDown`; no discovery failure is retried automatically.
Official-endpoint capacity metadata supplements only exact discovered model IDs;
custom endpoints never inherit those facts by model name alone.

The 1 MiB bound includes the Domain record envelope. If a backend write fails,
the API reports an unknown outcome and the owner closes further write admission
until Host restart reloads durable truth. Reads retain the last confirmed desired
revision with an explicit uncertainty diagnostic; it is never used for another
compare-and-replace while the commit outcome is unknown.

This ordinary Host plugin owns form-managed AI deployment configuration. One
`rsi.managed-providers` Storage document holds a desired revision and at most 64
closed provider definitions within 1 MiB. Each definition selects exactly the
existing OpenAI, OpenAI-compatible or DeepSeek factory. Those factories own
configuration validation; arbitrary Profile text, factories, native artifacts and
secret values are not accepted. Credential references must belong to the selected
provider's existing owner identity; arbitrary credential owners are rejected.

A fixed child Host catalog contains only those three linked Replayable factories
and inherits the parent's Language/Image registrars. Its ordinary ScopedProfile
owns deployment generations. Configurations are preflighted before the desired
document is durably published, then applied using that Profile's input updater.
The owner requires the enclosing Profile control publication, so the source
Profile completes its initial route publication before saved managed routes are
restored. Generic Host initial convergence can therefore return while this owner
is Pending; standard product startup waits for its Active Profile status.
Desired and successfully applied revisions remain separate through convergence,
rollback and uncertain replies. Startup creates the child scope and reconstructs
the saved desired configuration; provider activation failure remains a visible
configuration diagnostic so the management API can repair it. Malformed durable
configuration is rejected before provider activation.

One non-queued writer owns desired-revision CAS. The plugin retains admitted
writes through durable publication and convergence even if the caller disappears.
Every mutation also holds the shared configuration grant lease, including existing
Settings writes elsewhere. Retirement closes admission, drains writes, then closes
the child Profile. Provider request lifetimes remain with the existing routers.

User-Profile deployments are not generated or edited here. The Models catalog
remains the authority for currently callable routes; a collision with a source-owned
deployment fails ordinary Profile convergence and preserves its source ownership.
