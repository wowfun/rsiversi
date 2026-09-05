# rsi-agent-presets

This package owns the bounded process-local catalog and authoring interface for
Agent presets. A preset is one directory named by a validated `AgentPresetId`,
with a required regular, no-follow `agent.profile.toml` and optional bounded
`preset.toml` display metadata. Explicit roster discovery is fresh on every
call. It scans system authority, configured roots in declaration order, and an
optional user root in that precedence order; the first row for an id wins even
when it is broken. System authority may be either a root or one exact injected
id/directory pair; an exact pair never discovers or grants System trust to a
sibling directory. Selection probes only the exact `<root>/<id>` path in the
same precedence order and stops at the first real directory. It does not
compile sibling presets; a failed exact selection enumerates only bounded valid
directory ids for its diagnostic. `compile` compiles the selected source once,
while `document` returns bounded text only when the root document remains
byte-identical across its selected-source preflight. The application injects one `AgentPresetProfileCompiler`
whose frozen paths, platform, defines, limits, and Agent-only contribution-id
allowlist are shared by roster preflight and generation compilation. The
allowlist is not the Host factory catalog and contains no executable factory. A
roster row is healthy only when the complete Profile source,
including required includes and pure expressions, compiles semantically and
every enabled leaf belongs to that allowlist. Agent-forbidden Local or event
isolation is rejected by the same pure preflight. Valid-id directories with a
missing, non-regular, oversized, non-UTF-8, syntactically invalid, or
semantically invalid composition remain visible with a bounded categorical
reason instead of disappearing. Reasons do not contain source text, expression
text, source paths, or parser diagnostics. Metadata never controls health or
trust. A display name or description is trimmed and accepted only when it is
non-empty and contains no Unicode control character, so every text transport
receives single-line terminal-safe metadata.

Roster rows are path-free and expose id, root source, independently assigned
trust, effective-default status, display metadata, and health. `document`
returns the same source and trust with compilation-window-stable bounded text;
`compile` rebuilds the selected source through the same frozen compiler and is
the candidate-producing seam used by Agent composition. `location` is the
separate Local-only filesystem authority. The deployment supplies a validated
base default, and an injected default store may layer one
syntactically valid identity over it. Selection does not require current
discovery or health; generation resolution reports a missing or broken source
when a fresh session actually needs that preset. Selection is explicitly
unavailable when no persistence adapter was injected.

Authoring is copy-only. `copy` clones one selected preset directory
into the explicit user root. Without a name override, bounded `preset.toml`
bytes are preserved exactly even when discovery ignored malformed display
metadata. A name override requires valid known metadata fields, changes only
`name`, and preserves `description` and `order`; metadata remains independently
bounded to 16 KiB. Where descriptor-relative symlink reads are available,
relative symlinks are resolved from a captured source-root descriptor and
materialized as owned content; absolute, escaping, or cyclic links and special
files are rejected. Copy enforces depth/entry/aggregate-byte bounds before
publication, writes owner-only modes, and atomically publishes without replacing
an occupied id. A failed copy leaves no roster row. Fallback platforms currently
reject symlinks instead of materializing them; native Windows handle-relative
materialization remains an integration requirement. `delete` accepts only a
real preset directory owned by
the explicit user root; configured roots remain read-only even when their trust
is `user`. It removes the row from discovery atomically before recursive cleanup
and clears a matching user-default override first. A user-owned deployment base
default cannot be deleted because clearing an override cannot replace that base.
Existing Agent generations are outside this package and therefore remain
unaffected.

On Unix, opening the explicit user root accepts only an operating-system alias
in the first path component directly below `/`. That component is canonicalized
once, the untouched suffix is then traversed from `/` with directory-relative
`O_NOFOLLOW`, and every deeper symbolic link is rejected. Opening errors report
the caller's logical path, while all mutations use the opened directory
authority. The same owned-root interface is used by copy and delete and by the
standard product's built-in preset cache.

The fixed limits are 32 roots, 256 roster rows, 16 KiB metadata, copy depth 32,
256 traversed filesystem entries, and 16 MiB copied bytes. The injected Profile
compiler owns its source, include, expression, and candidate bounds. Catalog
health deliberately stops before concrete factory resolution, preparation, or
Runtime mutation. Standing generation activation, session binding, CLI
transport, and cross-process authorization belong to their owning modules.
Generation compilation and resolution remain authoritative because roster
health is only a point-in-time discovery result.

The Session Profile fragment binds one absolute Store root, then starts Kernel
and one executor generation. The fragment serializes `maximum_active_turns` and
defaults it to one for deterministic embedding; the executor factory is the
sole owner of its accepted bound. The standard product explicitly composes
four.
Host Profile patching replaces the complete executor configuration and is the
standard product's user-facing override. The standard product registers those
factories and composes the fragment into its one shared local Host.
