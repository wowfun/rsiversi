# rsi-apply-patch

This capability family owns structured patch validation, descriptor-relative
filesystem mutation, the hidden helper process protocol, and the model-facing
`apply_patch` tool. It is independent of shell selection: patch execution does
not pass through Bash, PowerShell, or a generic coding-tools bundle.
The helper response is a closed protocol: every reported filesystem effect is
one of `add`, `update`, `delete`, `move_write`, `move_delete`, or `mkdir`;
unknown effect kinds are rejected before the response becomes a Tool result.
Malformed model arguments return a bounded `invalid_arguments` Tool result
before helper admission. Its diagnostic describes the expected schema without
echoing untrusted keys or values.

After helper admission, missing, malformed or incomplete settlement evidence
returns `ToolError::OutcomeUnknown`. This includes cancellation after start.
It interrupts the owning Turn without a model-facing error result or automatic
replay. A complete validated helper response still reports its exact applied
or rejected effects through the ordinary result contract.
