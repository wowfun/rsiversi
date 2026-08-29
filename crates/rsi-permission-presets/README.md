# rsi-permission-presets

`rsi-permission-presets` is an ordinary plugin that publishes a frozen,
exact-name map of sandbox and approval defaults. It does not enforce either
policy and cannot grant authority by itself.

Consumers choose an explicit name. Missing or duplicate names fail loud; there
is no guessed preset or deep merge.
