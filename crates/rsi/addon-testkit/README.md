# rsi-addon-testkit

Public addon lifecycle assertions run solely through StandardAddonSet, HostBuilder,
Profile updates and nominal Local contracts. Authors supply their frozen addon
catalog, one explicit role, two valid Profile programs and a semantic probe.
The scenario activates the first generation, invokes its exported service,
replaces the Profile, invokes both retained and current services, verifies a new
service identity and checks complete Host teardown.

This is a deterministic library, not a product runtime or fixture provider. It
does not discover addons, inspect private tables, load credentials, or open user
state. The probe owns addon-specific behavior assertions; each invocation should
use isolated fixture resources. Tools, UI and Agent-specific tests additionally
exercise their owning public contracts. Linked consumers pin this package to the
same full Git revision as every other RSI SDK dependency.
