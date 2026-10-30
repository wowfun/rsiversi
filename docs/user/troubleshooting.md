# Troubleshooting

[简体中文](../i18n/zh-CN/user/troubleshooting.md)

This reference covers source installation and local Web/TUI/Desktop. Start with the exact
error from the owning terminal. Include the OS, shell, revision and command when
reporting a problem; remove API keys, launch tickets and device tokens.

| Symptom | Next step |
| --- | --- |
| Optional package missing | Install it with `rsi plugin install NAME` or an explicit local package. See [installation](source-install.md). |
| Source build prerequisite missing | Install the prerequisites for that producer from [installation](source-install.md), then rerun the build. |
| PowerShell refuses the script | Use the process-level command in [installation](source-install.md); organization policy may require its administrator. |
| Node or pnpm uses a wrong version | Select the versions declared by the Web package and its lockfile before building Web. |
| Long-path failure | Move the Git checkout to a shorter path on local NTFS. Check Git long-path configuration and OS policy. The script changes neither. |
| Build failed | Inspect the producer output or the bounded source-build diagnostic. A failed watched build retains the last admitted package. |
| Port in use or denied | Stop your previous instance, or launch Web with `--port 0` for a free port. Use the newly printed URL. |
| Browser did not open | Copy the full launch link or use `--no-open`. Do not copy only the origin on first login. |
| Login link no longer works | Run `rsi web` again to obtain a new link. A ticket is single use and expires after ten minutes; see [Web launch](../../apps/serve/README.md). |
| Configuration is read only | Reopen through the current local launch link. A remote device needs an explicit configuration grant. |
| No default model | Save credential, provider and default separately, then start a new conversation. |
| Authentication rejected | Check the provider, credential slot and account key; local credential presence is not a remote validation. |
| Unknown model or protocol error | Check the exact model identifier, endpoint and protocol with the provider. |
| Host already owned | Normal applications share the resident Host. For maintenance, use `rsi host stop` with the same homes. Do not delete the owner lock; see [Host ownership](../../crates/rsi/service-host/README.md). |
| TUI refuses mintty/conhost | Open PowerShell or Git Bash inside Windows Terminal. Do not wrap it in winpty; see [terminal requirements](../../apps/terminal/README.md). |
| Workspace rejected | Windows workspaces and private state require fixed local NTFS; network roots, ReFS Dev Drives and exFAT are unsupported. See [platform support](../../crates/rsi/README.md). |

## Windows capability limits

Web, TUI, Desktop and ACP share the resident [Service Host](../../crates/rsi/core/README.md).
The Web directory browser is unavailable; type the workspace path.

Restricted shell execution is unavailable. The AppContainer feasibility probe is
separate from the shipped Sandbox backend; full-access execution is not a substitute
for a successful sandbox check. Workspace review requires a strong source reader and
is unavailable on Windows.

Manual Web terminals use native PowerShell through ConPTY with the current user's
OS permissions. The terminal footer shows this access; Agent permissions are
unchanged. Interactive input, resize and process-tree cleanup are supported.

Linux SSH targets, Browser/Automation,
completed process output cache and MCP Profile management are
also outside this Windows release scope. A visible action may return a specific
unavailable diagnostic. Use Linux/WSL for those supported Linux workflows; do not
interpret the Windows build as full platform parity.
