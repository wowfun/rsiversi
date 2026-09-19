# rsi-mcp-protocol

Configuration explicitly enables at most eight server identities and selects raw
server Tool names. HTTP URLs are absolute HTTP/S endpoints without user information
or fragments. Optional bearer credentials and stdio environment credentials use
owner-local `rsi.mcp` references. Stdio programs and working directories are absolute,
with explicit argv and non-ambient environment. Configuration and request values
are bounded before execution; no annotation changes authorization.

A complete frozen manifest retains selected flags, original server Tool definitions,
resource descriptors, protocol version, capabilities and attributed instructions.
It contains at most 64 advertised Tools and 256 resource entries (including attributed instructions) across its servers;
actual registration also obeys the Tool Runtime's aggregate 64-tool ceiling. Each
manifest must fit the existing 256 KiB Domain state limit and the complete Session
baseline's 1 MiB limit. Nothing is truncated, sharded or replaced by a digest-only
record. The server target digest includes its explicit non-secret configuration;
public Tool names always contain a tuple-derived suffix: the ASCII-normalized
`mcp__{server}__{tool}` prefix truncated to 51 characters, an underscore, then
the first 12 hexadecimal SHA-256 digits of the canonical JSON `(server, tool)`
tuple. Names are at most 64 bytes and independent of catalog order or selection.
Final collision detection rejects any duplicate identity. Raw names are retained
independently for RPC calls. The manifest Domain uses codec version 2; older
codecs are explicitly unsupported, including saved empty manifests. Restoration
never renames saved Tools or substitutes a current catalog.

`validate_manifest_catalog` owns aggregate counts, strictly ascending unique
server identities, and public Tool-name uniqueness for both decoded manifests
and live frozen catalogs. It does not validate individual server schemas or the
encoded Domain byte limit; those remain required at their respective boundaries.

A manifest's Domain fork policy resets to the child generation's selected initial
manifest. Saved definitions determine restoration; current transport epochs and
verification determine whether execution can proceed. These are distinct facts.

The authoritative supported version list and `LATEST_PROTOCOL_VERSION` live in
`manifest.rs`. HTTP `x-mcp-header` annotations are validated before a complete
manifest is admitted: names are unique case-insensitively, primitive types are
restricted, and paths traverse only direct `properties` chains. Header projection
never grants a new endpoint or changes credential ownership.
