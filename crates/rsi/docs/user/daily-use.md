# Daily use

[中文](daily-use.zh.md)

A conversation belongs to the selected workspace and keeps its own saved history.
Choose an existing conversation to continue it, or **New conversation** for a
fresh context. Changing the default model affects new conversations.

Use the stop control to cancel an active response. Wait for its settled state
before retrying; closing a tab alone does not cancel work. File edits may already
have happened before cancellation, so inspect the workspace before repeating a task.

Browser reloads and additional tabs use the current browser cookie. If you clear
cookies, revoke the browser device, or lose an unused launch link, restart Web and
use its new link. The launch ticket expires after ten minutes and can be used once;
the [Web launch contract](../../../../apps/serve/README.md) owns this behavior.

## Stop and switch interfaces

Press Ctrl+C in the launching terminal and wait for exit. Windows runs the service
inside that process: Web and TUI cannot use the same Host directories simultaneously.
Stop one before starting the other. Closing a terminal forcibly can shorten cleanup;
prefer Ctrl+C when possible. See the [Host ownership contract](../../core/README.md).

To revoke a browser on Windows, stop Web first. In PowerShell:

<!-- rsi-example id=devices-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command '& (Join-Path (& .\rsi.ps1 path) rsi.exe) --profile devices list'
```

Then pass the listed device ID to `--profile devices revoke DEVICE_ID`.
Use a PowerShell process started with `-ExecutionPolicy Bypass` if scripts are
restricted, as described in [installation](source-install.md). Never paste a
device registration receipt into a public issue; it can contain a token.
On Linux, device administration connects to the existing Service.

## Find your data

Windows uses the standard per-user application config/state/cache locations with
an `rsi` directory. Linux uses the corresponding XDG locations. Explicit
`XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME` overrides must be absolute;
RSI appends `rsi` beneath each of them on both platforms.
Use the same locations when reopening history. Back up config and state while
the application is stopped; keep credentials private. Do not delete these directories
to fix a build failure: build outputs live under the checkout's `target` directory.

The [Host path contract](../../core/README.md) owns exact path resolution.

