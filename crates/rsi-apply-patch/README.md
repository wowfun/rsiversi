# rsi-apply-patch

This capability family owns structured patch validation, descriptor-relative
filesystem mutation, the hidden helper process protocol, and the model-facing
`apply_patch` tool. It is independent of shell selection: patch execution does
not pass through Bash, PowerShell, or a generic coding-tools bundle.
The helper response is a closed protocol: every reported filesystem effect is
one of `add`, `update`, `delete`, `move_write`, `move_delete`, or `mkdir`;
unknown effect kinds are rejected before the response becomes a Tool result.
