# rsi-credentials-local

This ordinary plugin resolves exact credential references from an injected OS
keyring store first and an explicitly captured startup environment snapshot
second. Profile configuration maps references to allowed variable names but
cannot contain values. Administrative writes are rejected while an environment
fallback exists, so an unset operation cannot silently reveal a different
secret source.

The default factory uses the platform keyring. Tests inject a memory
`SecretStore`; they never touch a real keyring or ambient environment.

Provider configuration owns one blocking-store admission boundary shared by
resolve, set, and unset. `maximum_concurrent_store_operations` defaults to 8
and accepts 1 through 64. Calls
for one full credential reference share one in-flight lookup; settled results
are not cached. `resolution_timeout_ms` defaults to 30 seconds and accepts 1
millisecond through 5 minutes. A timeout detaches only that waiter; admitted
synchronous work keeps its permit until the backend returns. Distinct work
waits for admission before creating a background task or singleflight entry,
so timed-out queued callers cannot leave an unobserved backlog.
Administrative writes have no waiter timeout: they wait for their exact Store
result, and a dropped caller cannot release the permit while its blocking
closure is still running.
