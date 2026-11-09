# rsi-sandbox

Durable enforcement stamps validate their originating host's paths with the
[normalized host-path grammar](../../rsi-workspace/path/README.md). This does
not authorize native access; the selected provider verifies its actual paths.

This package owns platform-neutral sandbox modes, explicit process inputs,
confined process plans, enforcement stamps, and the Local service contract. It
contains no feature probing, process spawning, filesystem mutation, or plugin
lifecycle.

`ProcessPlanOwner` retains native resources through child settlement. A Windows
issuer may attach an owned primary token and its private temporary
directory; cloning a plan shares those resources. Adding an execution lifetime
pin preserves the launch resources. Token preparation and directory grants
remain with the issuer, not this contract.
`ConfinedProcess::prepare_environment` applies a selected private directory to
both Windows `TEMP` and `TMP` before the caller freezes its complete environment.
It reads no ambient environment and leaves unrelated entries untouched.
Its return value identifies whether a selection was applied, so callers only
recheck environment bounds when Sandbox changed the already validated input.

`WorkspaceReadRequest` carries the exact orchestrator-selected paths and mode.
Its scope is process-local, has no deserializer, and exposes immutable values
plus an opaque provider generation. Providers allocate one generation per service
instance; clones compare equal and independently allocated generations differ.
Scope construction validates native absolute normalized bounded paths and a cwd
within the workspace. The scope supplies no API authorization or approval bypass.

An enforcement stamp is a closed semantic combination. Unconfined evidence is
valid only for danger-full-access with host scratch and network; Bubblewrap
evidence ordinarily uses private `/tmp` and host network. Its separate pipe-only
source-reader view requires ReadOnly filesystem, Host scratch and Isolated
network; no write-mode or PTY source-reader plan is supported. Landlock evidence is restricted, uses host scratch, and currently
retains host network. Unsupported or contradictory combinations are rejected
when durable data is decoded or revalidated.

Windows ACL evidence uses `WindowsAclReadOnly` or `WindowsAclWorkspaceWrite`,
not the read-only mount evidence. Both use a Low-integrity write-restricted
token containing the logon SID and Everyone, required by Windows initialization
and PowerShell's crypto runtime. Existing writable Low-integrity objects whose
ACL admits those ambient SIDs remain writable. Workspace-write additionally
carries its workspace capability. Both restricted modes carry a distinct
private-directory capability and use PrivateDirectory scratch; read-only never
carries a workspace write capability. Writable private scratch lets PowerShell
evaluate its real AppLocker policy instead of treating failed temporary policy
probes as language restrictions. Network remains Host. PrivateDirectory means an owned
directory selected through TEMP/TMP, not a separate filesystem namespace.
The stamped workspace is also revalidated as an absolute, lexically normalized
path: relative paths and paths containing `.` or `..` components are rejected
without requiring the durable path to exist or re-canonicalizing the live
filesystem. A restricted stamp cannot name `/` or `/tmp` itself as the
workspace, matching the request boundary shared by all local restricted
backends.

`ProcessRequest<P = PathBuf>` preserves native defaults while allowing an execution
owner to carry its own opaque resolved-program value. Mapping the program moves
all policy, argv and workspace fields unchanged; native Sandbox still receives
an exact native path and owns native validation.
