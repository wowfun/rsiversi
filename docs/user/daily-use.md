# Daily use

[简体中文](../i18n/zh-CN/user/daily-use.md)

A conversation belongs to the selected workspace and keeps its own saved history.
Choose an existing conversation to continue it, or **New conversation** for a
fresh context. Changing the default model affects new conversations.

Use the stop control to cancel an active response. Wait for its settled state
before retrying; closing a tab alone does not cancel work. File edits may already
have happened before cancellation, so inspect the workspace before repeating a task.

Browser reloads and additional tabs use the current browser cookie. If you clear
cookies, revoke the browser device, or lose an unused launch link, restart Web and
use its new link. The launch ticket expires after ten minutes and can be used once;
the [Web launch contract](../../apps/serve/README.md) owns this behavior.

## Stop and switch interfaces

Windows and Linux applications using the same config and state homes share a
resident Host. TUI, Web and Desktop can run together. Closing an interface detaches
that client; it does not stop the Host or cancel another client's response.
Use Ctrl+C to leave a terminal interface and allow its cleanup to finish.

`rsi web` opens a new launch link and returns. `rsi web status` inspects its
listener; `rsi web stop` stops that listener. `rsi host stop` stops the shared
Host and disconnects all its clients. See the
[Host ownership contract](../../crates/rsi/core/README.md).

Device administration connects to the same Host. For example, in PowerShell:

<!-- rsi-example id=devices-windows platform=windows -->
```powershell
rsi run devices -- list
```

Then pass the listed device ID to `rsi run devices -- revoke DEVICE_ID`.
Never paste a device registration receipt into a public issue; it can contain
a token.

## Find your data

Windows uses the standard per-user application config/state/cache locations with
an `rsi` directory. Linux uses the corresponding XDG locations. Explicit
`XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME` overrides must be absolute;
RSIversi appends `rsi` beneath each of them on both platforms.
`--config-home`, `--state-home` and `--cache-home` select final directories without
appending `rsi`; relative paths resolve from the calling directory. Use the same
locations when reopening history. Stop the Host before backing up config and
state; keep credentials private. Do not delete these directories
to fix a build failure: build outputs live under the checkout's `target` directory.

The [Host path contract](../../crates/rsi/core/README.md) owns exact path resolution.
