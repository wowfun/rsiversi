# rsi-client-preferences

An ordinary Settings contribution owns the `rsi.client` namespace. Its closed
value contains `web.enter_submit` (default false) and `tui.enter_submit` (default
true). Defaults and validation have one owner here. The standard Service Host
registers the factory as `rsi.client.preferences`; the generic Settings editor
discovers and writes it using existing version CAS and durable persistence.

Clients capture the value once per application lifetime: reconnect Web or restart
TUI to apply an accepted edit. Changing Sessions does not change the input mode.
Metadata explicitly reports restart application timing. A missing registration
uses defaults, allowing compositions to omit this contribution; malformed values
or read/transport failures are errors rather than silently ignored settings.
Standard native connections forward SettingsAccess from the selected embedded or
Unix socket Host, matching the HTTP client. Minimal embedded TUI compositions may
omit the Settings reader entirely and use defaults.

Web always supports Ctrl/Command+Enter and Shift+Enter, and suppresses keyboard
submission during composition. TUI always supports Ctrl+S for NextTurn and
Ctrl+J for a newline. The preference controls plain composer Enter only; menus,
question answers and contributed form editors retain their own Enter behavior.
No Session Header, model selection or accepted message identity is changed.
