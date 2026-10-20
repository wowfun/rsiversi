# Use the terminal interface

[简体中文](../i18n/zh-CN/user/terminal.md)

The TUI provides fullscreen conversations and model setup. Finish
[source installation](source-install.md) first. On Windows, use **Windows Terminal**
with PowerShell or Git Bash; mintty and classic conhost cannot run this TUI.
Stop Web before opening the same Host on Windows. These examples use the Web package from the source-install tutorial; after a separate TUI build, omit `--from web`.

<!-- rsi-example id=tui-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1 tui --from web
```

<!-- rsi-example id=tui-bash platform=git-bash -->
```bash
bash scripts/rsiversi.sh tui --from web
```

An unconfigured account opens the home/setup screen. Use `/login deepseek` to
configure credentials and `/model` to select a model. `/help` shows available
commands. `/new` starts a conversation; `/resume` opens saved conversations.

Bracketed multiline paste inserts one draft; inspect it before sending.
Terminal-specific key combinations follow the [TUI interaction reference](../../apps/terminal/docs/tui-design.md).
Use `/quit` or `/exit` to leave. Ctrl+C exits from Home, including when a draft
is present. Unsaved drafts are discarded at exit.

The Windows terminal backend restores the console modes and cursor on normal
exit and handled cancellation when the terminal host responds. A forced process
kill or an unresponsive terminal host cannot promise complete cleanup. Restart
the terminal tab if either leaves it altered. The
[terminal ownership contract](../../apps/terminal/README.md) defines cleanup guarantees.
