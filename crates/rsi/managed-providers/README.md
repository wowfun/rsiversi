# rsi-managed-providers

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
