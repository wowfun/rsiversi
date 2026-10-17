# Use the terminal interface

[中文](terminal.zh.md)

The TUI provides fullscreen conversations and model setup. Finish
[source installation](source-install.md) first. On Windows, use **Windows Terminal**
with PowerShell or Git Bash; mintty and classic conhost cannot run this TUI.
Stop Web before opening the same Host on Windows.

<!-- rsi-example id=tui-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\rsi.ps1 tui
```

<!-- rsi-example id=tui-bash platform=git-bash -->
```bash
bash rsi.sh tui
```

An unconfigured account opens the home/setup screen. Use `/login deepseek` to
configure credentials and `/model` to select a model. `/help` shows available
commands. `/new` starts a conversation; `/resume` opens saved conversations.

Bracketed multiline paste inserts one draft; inspect it before sending.
Terminal-specific key combinations follow the [TUI interaction reference](../../../../apps/terminal/docs/tui-design.md).
Use `/quit` or `/exit` to leave. Ctrl+C exits from Home, including when a draft
is present. Unsaved drafts are discarded at exit.

The Windows terminal backend restores the console modes and cursor on normal
exit and handled cancellation when the terminal host responds. A forced process
kill or an unresponsive terminal host cannot promise complete cleanup. Restart
the terminal tab if either leaves it altered. The
[terminal ownership contract](../../../../apps/terminal/README.md) defines cleanup guarantees.
