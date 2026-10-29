# Product launch

The launcher freezes `--config-home DIR`, `--state-home DIR` and `--cache-home DIR`
before selecting an application. These are final roots; unlike XDG environment
overrides they do not append `rsi`. Relative command-line roots resolve from the
invoking directory. Unspecified roots use the ordinary platform defaults.

`<config-home>/rsi.toml` is a bounded versioned launch document. It references
Profiles rather than duplicating their ordered plugin grammar. Document-relative
paths resolve from the document's directory. The absent default document means
the built-in TUI/standard Service configuration; an explicitly selected config
home requires a document. `config init` creates one without overwriting, and
`config show`/`validate` never activate plugins.

Settings and Credentials retain their separate providers below config-home.
The launch document is not a Settings backend. Changing one home does not silently
change any other home; independent environments select both config and state.
The product does not infer a development mode from the executable path.
Ordinary sessions and Profile previews both capture the caller’s user Home for
user instruction, agent and skill discovery; explicit product homes do not
replace that separate discovery root.

Global options precede the command. Application arguments after `--` are opaque
to the launcher. A bare command selects the configured default application;
`run NAME` selects an Application Profile explicitly.

Resident launch publishes a private immutable copy of the actual executable,
streaming its SHA-256 and bytes with a 4 GiB ceiling. This accommodates ordinary
Linux debug executables containing DWARF data as well as release builds. Neither
the selected homes nor the runtime behavior depend on the build profile.
After successful readiness, the startup coordinator reclaims older runtime
generations and abandoned publication files. It keeps the admitted running
generation and refuses to follow links or delete unrecognized children. A
failed cleanup reports its error without stopping the successfully started Host.
An unrecognized generation is retained and reported; it does not prevent
reclamation of other recognized generations in the bounded scan.
Application transitions have a three-minute deadline, including startup
coordination, immutable runtime publication and resident Host readiness. The Unix native-addon bootstrap
uses that same product deadline. Generic Meta defaults, service-call deadlines
and shutdown deadlines are unchanged; a large debug executable must not consume
the entire generic thirty-second plugin budget before the Host can become ready.

Resident configuration observation is a connection-owned lease. Starting a Host
does not permanently enable watching. The ordinary product management plugin
shares one leased policy with Profile and native-addon observation; a client
retains its lease for its watch lifetime. Management admits at most 32 control
connections, of which at most 16 may retain watch/build leases, leaving capacity
for commands. Excess connections or leases receive bounded errors.
The same management interface performs
explicit `host reload` on Windows and Linux, returning the accepted convergence
outcome. The fixed Lifecycle stop/status interface remains independent.
Attachments compare the caller's tool environment and credential fallbacks
against that Host's frozen inputs. A mismatch produces a categorical warning
without variable names, values or fingerprints; restart explicitly to adopt new
environment inputs. Durable Credentials keep their usual precedence.

Package references resolve named Profile exports from `<state-home>/packages`.
Application catalog offers may repeat an identical core interface, but cannot
claim a different version or schema under the same name.
The source retains the selected immutable dependency closure for its complete
runtime lifetime. Installing a newer package changes subsequent resolutions;
it does not replace files already used by a running Profile. `plugin list`
reports installed selections; `plugin install --local DIR [DIR ...]` admits
explicit manifests atomically, without activating their Profiles or running
package code.

`plugin install NAME [NAME ...]` downloads from the configured `[repository]`
(`name`, `url`, Ed25519 `public_key` in lowercase hex), or the release's compiled
repository identity when no override is present. Source builds without a release
identity require an explicit repository or `--local`; they do not trust an
invented default key. Repository URLs require HTTPS, except explicit loopback HTTP
for local repositories. Redirects, URL credentials, query strings and fragments
are refused. Signed metadata is checked before following any manifest or file
reference. Downloads are bounded, hashed, staged privately and published together
with their rollback receipt. `plugin uninstall NAME [NAME ...]` refuses to break
remaining dependencies; `plugin gc` reclaims only unused generations.

Reset is an exclusive maintenance operation, not recursive home deletion.
Reset options are global and precede the application or command. Host stop/status/
reload and Web stop/status reject reset flags before maintenance. Help does not
resolve homes or perform maintenance. Tokens after the application, including
`--reset-state`, remain that application's literal arguments.
`--reset-config` archives launch/Profile selection and Settings while preserving
credentials, instruction and skill files. `--reset-state` archives session/runtime
data while preserving installed packages and trust metadata. Locks and recovery
journals remain in place. A live owner must be explicitly restarted or stopped
before reset; invalid launch configuration cannot prevent maintenance.

Maintenance takes the startup coordinator and both home leases before reading
or writing any reset journal. Configuration reset includes `rsi.toml`,
`settings.json`, `application-profiles` and `host-profiles`. State reset includes
the standard Agent, Media, ACP and Automation roots and the base SQLite database
with its WAL/SHM sidecars. Arbitrary files, explicit external Profile sources,
credentials and installed package/trust stores are outside this allowlist.

Each selected existing entry is renamed to a unique `.reset-<id>-<name>` sibling
on its own volume. A bounded journal is flushed in both home authorities before
the first rename. Recovery validates the exact home pair and fixed entry list,
then validates every source/backup pair before the first rename and completes
the recorded operation; ambiguous source/backup pairs fail
closed. Completion receipts remain in `session-host`. Reset never removes old
backups. A reset configuration receives a new default launch document only
after every selected old entry has been archived.
Fixed participant lookups do not enumerate sibling backups, so retained reset
history cannot exhaust the operation's directory-entry budget.
On Windows an existing entry must belong to TokenUser; its inherited ACL is
made private before archival, without changing ownership or file contents.
Maintenance fixtures create payload files and directories through the native
private creators so elevated test tokens also satisfy this ownership contract.

Source package builds are leases of the ordinary Host management plugin. A
request freezes the absolute source root, resolved native executable, arguments,
explicit inputs, package output and scrubbed caller environment. Inputs are read
through the retained directory authority (1024 entries, 16 MiB per file, 64 MiB
total); output directories cannot overlap watched inputs. Eight distinct builds
may be retained, and identical requests share a loop. A different recipe cannot
own an overlapping output directory. This conflict has a typed busy response;
watch launchers retry it for at most ten seconds while the prior lease drains.
Last-lease retirement cancels the Process
group and waits for settlement; idle hosts own no source polling loop.
Output conflicts use the canonical source root and, on Windows, a conservative
case-folded path key. Alternate casing cannot obtain a second writer or bypass
the separation between observed inputs and output directories.

A build requests explicit unconfined Sandbox execution, a thirty-minute deadline
and bounded 64 KiB output tails. Publication checks the same input fingerprint
after exit zero and immediately before atomically selecting the ordinary package.
Failure or cancellation preserves the selected good generation. Core and TUI
publication reports restart-required. Changed recipe bytes require a new lease;
changes to the source inputs alone trigger a new build. The watch launcher also
re-reads `rsi.toml`: valid application or source-recipe changes drain the current
attachment and replace its source leases. Invalid edits retain the running
configuration with a diagnostic. Changing the service selection requires an
explicit Host restart; it is not silently applied to an existing Host.

Source publication retries transient package-writer contention for at most five
seconds while continuing to check cancellation and input identity. Exhaustion
preserves the old selection and reports a categorical failure. Executable PATH
resolution preserves invocation spelling: resolving cargo's symlink to rustup
would otherwise change argv[0] and execute a different command.

An optional `sources.web.frontend` contains `command` (literal executable and
arguments) and `port` (default zero). It uses the source recipe's directory and
the same frozen, scrubbed caller environment. Without `--watch` it is inert.
The application owns this process through ordinary Process/Sandbox plugins;
the resident Host does not inherit the caller's display or frontend environment.
The frontend's bounded control handshake reports its actual loopback Origin
before a connection-owned backend is created. Proxy readiness precedes browser
or Desktop launch. Retirement closes control input, then bounds graceful exit
and waits for the managed process group to settle.
Published application-package selections are re-resolved after the running
application drains. Web renderer updates publish through the existing listener;
bootstrap changes require a new application attachment. Core and linked TUI
changes remain explicit restart-required outcomes.

Core replacement is an explicit `rsi core upgrade` operation, optionally selecting
an operator-trusted local package with `--local DIRECTORY`. Network upgrades
require the executable's compiled repository name and key. A configured mirror
may change only the URL; unpinned builds require an explicit local package. Package admission
precedes stopping the selected Host. The [installation owner](../../installation/README.md)
defines fixed-layout records, transaction versions, installation lease order and
platform filesystem policy. The
Host startup coordinator remains held across stop, executable
promotion and readiness. Executable hashes, final config/state homes and bounded
single-component backup names are recorded before the first rename. The previous
binary is retained on the same volume. Windows permits renaming the running
launcher image; the Host already runs from its immutable artifact copy.

Readiness across core revisions uses frozen Lifecycle v1 and native child
identity, rather than an old client's product API schema. Failed activation
restores every previous participant and the previous Host's running state. An
interrupted operation requires `rsi core recover` with the recorded homes; if the
primary executable name is temporarily absent, run the retained previous binary
with that command. Unexpected file contents or a different home authority are
refused before promotion or recovery. Successful transactions retain their
receipt. Once settled, recorded previous/candidate/failed executable copies are
reclaimed under the installation lease by their validated receipt names and
ordinary-file types; v2 additionally verifies each disposable payload digest. Windows
may retain an executing image until its caller exits; the next upgrade retries
that cleanup. Before a new transaction, abandoned candidate copies that have no
journal or receipt are also reclaimed. Pending recovery participants are never
collected. Runtime publication converts OS-canonical local-drive spelling to the
bounded native path representation before acquiring directory handles. There is
no background updater.

`rsi import --config-home OLD_CONFIG --state-home OLD_STATE` explicitly copies
an existing product home pair into the final homes selected by global options.
`--dry-run` checks paths, bounds and configuration syntax, and reports selected
entry names without creating directories, locks or guards, or changing ACLs.
It is a read-only preview, not an exclusive snapshot; actual import repeats all
checks under leases. SQLite verification follows staging in
the actual import. Both source and destination Hosts must be stopped; their startup and
home leases remain held through the operation. Source and destination roots
cannot overlap. The import does not follow links, scan other installations,
translate database schemas, or rewrite external Profile/Settings paths.

Configuration admission covers `rsi.toml`, `settings.json`, `application-profiles`,
`host-profiles`, `agent-presets`, `credentials`, `AGENTS.md`, `agents` and `skills`.
State admission covers the same owned data roots as reset. Packages, native
addon binaries, caches, runtime metadata and arbitrary files are not imported.
Missing source entries leave their destination counterparts unchanged. A missing
source launch document produces the default document only if the destination
also has none. Settings and launch syntax use their owning validators;
imported Agent Stores must pass the SQLite owner's offline verification.

Before copying, import records a bounded staging intent in both home authorities.
An interrupted preparation is fenced from ordinary launches; `rsi import recover`
discards only its named, incomplete staging trees, without changing destination
payloads. Synchronous preparation failures perform the same cleanup. Once a final
journal or receipt exists, recovery completes that transaction instead. Links and
unexpected staging types are refused, retaining the intent for inspection.

Before publishing any entry, import bounds and hashes every selected tree
(16,384 entries, depth 64, 4 GiB per file and 16 GiB overall), copies it to private
same-volume staging at the destination, and rechecks the source fingerprint.
Only offline SQLite verification can change staged bytes and therefore replaces
its staged fingerprint. Settlement validates the whole topology once under the
home leases before any rename, then carries those classifications through the
rename-only transaction; every recovery invocation validates fresh bytes.
An immutable journal of at most 64 KiB is published under both destination home
authorities before the first rename. Existing destination bytes are retained as
`.import-<id>-backup-<entry>` siblings. Recovery never replaces unexpected bytes.
`rsi import recover` completes a journal using its staged copies and the original
destination home pair; source availability is no longer needed. Other product
operations refuse pending imports, including reset. Completed receipts and
backups remain available. Imported files are still validated by their owning
providers when activated; successful copying does not grant new runtime authority.

Source polling reads bounded identity/size/change-time metadata every 300 ms.
Unchanged metadata skips payload reads, with a full content scan at least every
five seconds to catch changes hidden by filesystem timestamp granularity. Build
admission and publication always use fresh full content fingerprints; metadata
hints never authorize package publication.
