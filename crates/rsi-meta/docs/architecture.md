# rsi-meta architecture

## Product interfaces

[`CompositionProject`](../core/README.md) is the independently usable offline façade for candidate validation and lock creation. [`CompositionHost`](../core/README.md) is the only online embedded façade. The [CLI](../cli/README.md) owns the v0 control protocol and adapts it to host methods; transports never own composition state.

Inside the core crate, composition parsing, transaction recovery, persistence, routing, and runtime actor lifecycle are crate-private deep modules. The registry remains the only graph writer, routing publication remains one atomic snapshot replacement, runtime work remains actor-owned, and persistence shares one SQLite connection. The [loader](../loader/README.md) keeps one crate boundary while privately separating manifest/security validation, CAS staging, dynamic-library mapping, and mailbox ABI work.

## Online apply

```text
OperationId lookup and first input snapshot
              |
              v
validation and immutable CAS staging
              |
              v
process-fixed preflight -- yes --> durable RestartRequired only
              |
              no
              v
shadow prepare -- failure --> reverse abort
              |
              v
pair journal and file install (lock last)
              |
              v
active state, result, event, and revision commit
              |
              v
atomic routing publication
              |
              v
reverse retirement after generation leases drain
```

The graph revision advances only when a routing graph becomes active. A failure after durable commit but before publication terminates the host; a later `open` reconstructs the committed graph rather than continuing with split state.

## Process-fixed installation

```text
apply -> RestartRequired -> request_shutdown -> wait_terminated
      -> install_offline -> fresh open -> one activation
```

Preflight does not alter the installed pair or lifecycle. Offline install holds the same workspace lease as `open`, commits only files and its operation result, and never loads plugin code. A process that already mapped a different process-fixed artifact rejects replacement with `fresh_process_required`.

## Trust and protocols

Typed safe-Rust values are trusted only after validation at their owning boundary. Native plugin ABI frames, files, durable state, local transport input, and process inputs remain bounded and validated. Native plugins are trusted host code; [security.md](security.md) defines the failure domain. The [composition runtime](subsystems/composition-runtime.md), [configuration](subsystems/configuration.md), and [protocol reference](subsystems/protocols.md) own their current semantics.
