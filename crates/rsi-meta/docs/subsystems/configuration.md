# Configuration and package inputs

## Ownership

A composition manifest owns mounts, scopes, enabled state, instance configuration, and explicit provider bindings. A plugin package manifest owns package identity, host API requirements, target artifacts, provided and injected contracts, capabilities, `process_fixed`, and the optional configuration schema.

The exact external shapes are defined by the versioned [composition](../../../../schemas/rsi-meta/composition.schema.json), [plugin](../../../../schemas/rsi-meta/plugin.schema.json), and [lock](../../../../schemas/rsi-meta/lock.schema.json) schemas. The [loader](../../loader/README.md) owns validation not expressible in JSON Schema.

Composition, scope, and instance identities are 1-255 ASCII bytes beginning with a letter or digit and then containing only letters, digits, `.`, `_`, or `-`. Service contract names use the same bound and additionally allow `/`. Both schema validation and runtime manifest validation enforce these rules before identities reach graph state, logs, or persistence.

## Projects, candidate locks, and installed state

`CompositionProject` names a candidate manifest and optional lock independently of a live host. Validation resolves canonical package paths, target-specific artifacts, manifest hashes, artifact hashes, and configuration-schema hashes into diagnostics. `lock` atomically creates a missing lock, verifies normalized equivalent content as unchanged, and never overwrites a conflicting lock.

`CompositionWorkspace` fixes the installed manifest and lock paths. The CLI always uses `composition.toml` and `rsi-meta.lock` in its state directory; embedded callers choose both paths explicitly. Graph preparation uses candidate input without mutating the installed pair. Online hot apply and offline install journal both files and replace the lock last as the commit marker. Two absent files are an empty workspace; a lone installed file is invalid outside journal recovery.

Package manifests, schemas, and artifacts must be bounded regular files. Manifest-declared artifact and schema parent paths are physically resolved inside the package root, and the final component may not be a symlink. Ownership and restrictive-mode checks apply to security-sensitive host inputs such as the plugin cache and secret files, not to ordinary package files. Artifact content is hashed and staged without loading an unvalidated path.

## Instance configuration and secrets

Plain JSON configuration is validated against the package's schema. Only schema positions carrying `"x-rsi-meta-secret": true` may contain one of these reference shapes: `{ "$secret": { "env": "NAME" } }`, `{ "$secret": { "file": "/absolute/private/path" } }`, or `{ "$secret": { "keyring": { "service": "NAME", "user": "NAME" } } }`. An annotation at one `allOf` instance location composes with sibling constraints: the unresolved reference is accepted only there, while the resolved plaintext must satisfy the complete original schema. Plaintext at an annotated position and `$secret` objects elsewhere are rejected.

Secret resolution happens during configuration preparation. The canonical manifest and lock retain the reference rather than plaintext. Prepared plugin configuration receives the resolved value, while inspection, events, durable audit payloads, and hashes use a redacted projection.

File secret providers require bounded, owner-checked, non-symlink regular files. Keyring identifiers are bounded before lookup. Resolved secrets remain in process memory and inherit the product's [native trust boundary](../security.md).
