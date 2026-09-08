# rsi-settings-protocol

This package owns the runtime-independent Settings provider and consumer
contracts. Values are bounded JSON; schema validation is a caller-supplied
safe-Rust function and namespace revisions protect writes from stale clients.

Consumers may request an exact active namespace scope. Lookup does not register
a namespace or reveal unregistered raw document sections. A returned scope keeps
its original registration identity: retirement makes it stale even if a new
owner registers the same name. Reads and CAS writes share the owner's validator
and last committed value.

The package contains no files, environment access, plugin lifecycle, or global
registry.

Owning tests cover namespace syntax and bounds plus exact encoded-section byte
admission; provider and registry suites cover the stateful uses of those pure
validators.

`SettingsAccess` is the asynchronous namespace projection for clients. It exposes
read, replace and clear for an exact registered namespace, with no registration
or raw-document access. A snapshot includes an opaque registration identity and
a revision. Remote writes compare both: the identity changes on re-registration
and service restart, so revision zero from an old owner cannot authorize a write
to a new owner. Native `SettingsScope` already carries that identity implicitly;
its revision-only methods retain their generation fence.
Remote projections preserve common API errors, including uncertain write outcomes,
without exposing namespace registration or raw-document authority. Section sizing
counts bounded JSON before allocating an encoded payload.
