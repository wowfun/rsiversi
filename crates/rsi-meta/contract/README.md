# rsi-meta-contract

This package owns runtime-independent identities and value contracts shared by
Meta core, Profile, Host, and product protocol packages. It contains no Runtime,
Context, Fiber, executor, loader, or product implementation.

Linked and native resolvers construct `FactoryIdentity`; executable
`PluginFactory` implementations do not report their own identity. `PluginId`
names one statically registered implementation, while `InstanceId` names one
Profile application of that implementation.

The Runtime admission boundary validates nonempty bounded plugin/revision
provenance and requires native artifact provenance to contain an exact
lowercase SHA-256 digest before a Fiber can retain it.
