# rsi-client-preferences

An ordinary Settings contribution owns the closed `rsi.client` namespace with
`web.submit_key` (`enter` or `mod_enter`, default `enter`),
`web.busy_submit` (`queue` or `steer`, default `queue`), `appearance.theme` (`system`, `light` or
`dark`, default `system`) and `appearance.content_font_size` (integer 12 through
17, default 14). Values are shared by all clients of a Profile. The standard
Host registers `rsi.client.preferences`; persistence uses Settings version CAS.
Missing registration uses defaults; malformed values and read failures remain
errors. TUI input behavior is fixed by its [owner](../../../apps/terminal/README.md).
GUI saves apply immediately after Settings version CAS succeeds. Other GUI clients
refresh serially every two seconds, cancel reads on close and preserve their last
valid snapshot with a visible diagnostic after a read failure. Presentation-only
panel layout belongs to the document's separate device store, not this namespace.
Web always supports Ctrl/Command+Enter and Shift+Enter, and suppresses submission
during composition. Questions and forms retain their own input semantics.

Registration migrates the raw legacy section through the Settings owner. Either
legacy boolean becomes the new defaults, retaining appearance and unrelated
valid fields. Already migrated fields remain unchanged on later registration.
Malformed legacy values fail; read-only legacy stores require writable migration.
New API writes reject `enter_submit`. Migration never exposes raw provider access
to the GUI.

Enter submits by default, with Shift+Enter inserting a newline. While busy the
primary action uses the chosen delivery; Ctrl/Command+Enter chooses its opposite.
With `mod_enter`, plain Enter inserts a newline and Ctrl/Command+Enter always
chooses the primary action. The alternate delivery is also a touchable action.
