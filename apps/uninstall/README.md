# rsi-uninstall

The independent native maintenance entrypoint consumes
[`rsi-installation`](../../crates/rsi/installation/README.md), not full product
composition. It supports Linux and Windows. The installed platform launcher is
available without a source checkout. Windows PowerShell 5.1 copies the tool to a
private temporary directory for actual removal, verifies its digest and cleans
the copy after use; dry-run executes the installed tool in place;
Linux runs it directly. Neither removal nor purge invokes Cargo or downloads code.
The Windows digest check also binds the temporary copy to its admitted source.
Linux has no copied executable: it uses the tool admitted at installation/package
publication, or an explicitly trusted external tool, without adding a shell JSON
parser or another runtime dependency to re-validate the record before execution.

The scripts accept prefix, uninstaller and final config/state/cache homes plus
purge, dry-run, no-path, force, yes and help options. Force permits confirmed
fixed-file removal without a reliable record; it never means force-killing a Host.
An absent installation is an ordinary-removal no-op. Purging after the installed
tool is removed requires an explicitly supplied prebuilt uninstaller.

The native protocol has install/uninstall operations and an exact protocol-probe flag and requires
interface version 1. Installation is the bootstrap scripts' publication backend.
Help and protocol probes are side-effect free. Diagnostics use stderr and a
nonzero status without shell exception stacks. Platform launchers preserve native
exit status; Bash usage errors remain 2 and PowerShell usage errors remain 1.

Tests exercise the native entry and both script launchers against isolated
prefixes and homes, including missing or corrupt main programs,
partial removal, explicit external tools and shared homes.
