# Troubleshooting

[中文](troubleshooting.zh.md)

This reference covers source installation and local Web/TUI/Desktop. Start with the exact
error from the owning terminal. Include the OS, shell, revision and command when
reporting a problem; remove API keys, launch tickets and device tokens.

| Symptom | Next step |
| --- | --- |
| No published application | Run setup, then build. A normal launch never builds automatically. |
| Missing Rust, Node, MSVC or SDK | Run doctor; install the named prerequisite, reopen the shell, and repeat setup. |
| PowerShell refuses the script | Use the process-level command in [installation](source-install.md); organization policy may require its administrator. |
| Node or pnpm uses a wrong version | Run setup. The launcher uses the repository tool installation; no global pnpm replacement is needed. |
| Long-path failure | Move the Git checkout to a shorter path on local NTFS. Check Git long-path configuration and OS policy. The script changes neither. |
| Build failed | Inspect the reported generation's build.log. The previous publication remains selected. |
| Port in use or denied | Stop your previous instance, or launch Web with `--port 0` for a free port. Use the newly printed URL. |
| Browser did not open | Copy the full launch link or use `--no-open`. Do not copy only the origin on first login. |
| Login link no longer works | Restart Web to get a new link. A ticket is single use and expires after ten minutes; see [Web launch](../../apps/serve/README.md). |
| Configuration is read only | Reopen through the current local launch link. A remote device needs an explicit configuration grant. |
| No default model | Save credential, provider and default separately, then start a new conversation. |
| Authentication rejected | Check the provider, credential slot and account key; local credential presence is not a remote validation. |
| Unknown model or protocol error | Check the exact model identifier, endpoint and protocol with the provider. |
| Host already owned | On Windows, stop the other Web/TUI/Desktop/ACP or offline device process using these directories. Do not delete the owner lock; see [Host ownership](../../crates/rsi/service-host/README.md). |
| TUI refuses mintty/conhost | Open PowerShell or Git Bash inside Windows Terminal. Do not wrap it in winpty; see [terminal requirements](../../apps/terminal/README.md). |
| Workspace rejected | Windows workspaces and private state require fixed local NTFS; network roots, ReFS Dev Drives and exFAT are unsupported. See [platform support](../../crates/rsi/README.md). |

## Windows capability limits

Web conversations, model configuration, saved history and native Files use the
embedded service. The directory browser is unavailable; type the workspace path.
The [embedded service](../../crates/rsi/core/README.md) is not shared between independent application processes.

Restricted shell execution is unavailable. The AppContainer feasibility probe is
separate from the shipped Sandbox backend; full-access execution is not a substitute
for a successful sandbox check. Workspace review requires a strong source reader and
is unavailable on Windows.

Linux SSH targets, Web PTY, Browser/Automation,
Agent Store reset, completed process output cache and MCP Profile management are
also outside this Windows release scope. A visible action may return a specific
unavailable diagnostic. Use Linux/WSL for those supported Linux workflows; do not
interpret the Windows build as full platform parity.
