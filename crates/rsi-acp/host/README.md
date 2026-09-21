# rsi-acp-host

The ordinary Host plugin owns operator-configured external ACP endpoints and at
most eight resident peers, including preparing and retiring processes. Local
Profile configuration supplies exact executable, arguments, workspace and complete
environment. Credentials use `rsi.acp` references resolved only at launch. Neither
model Tools nor remote clients supply commands, environment or credential values.
Endpoint discovery returns identities and availability only. There is no installer,
PATH search, automatic reconnect or prompt retry.

An endpoint may declare ordered `session_options` entries with exact `id` and
string `value`. The client applies these through stable `session/set_config_option`
after new/resume/load, before publishing Ready or admitting a prompt. Model belongs
before effort when the former changes the available choices. Missing choices,
rejected changes or a response that does not confirm every applied value fail
setup and retire the peer. Configuration does not retry prompts or infer model
availability from a generic provider failure. Reconnection reapplies the selected
endpoint's configuration; it does not silently inherit a remote default.

The Host retains clients across UI detach. New conversation identities are allocated
by callers before admission and journal reservation; repeating start with the same
identity reads its observation instead of launching another remote Session. Resume
and full load are explicit operations over the saved endpoint and workspace. The
incoming route is installed before setup. Private MCP launch definitions use the
same explicit configuration and credential policy and are sent only to that peer.

Setup and cleanup tasks belong to the owner, including when an API waiter vanishes.
Local launch preparation has a 45-second deadline. After attachment, the Client
owns negotiation, configuration and close phase deadlines; the Host does not put
a shorter aggregate timer around them. Close waits for actual owner completion,
including transport cleanup, even when an outer API waiter has already timed out.
An explicit close, Host shutdown or provider retirement cancels setup or settles
prompt work, closes the transport and awaits Process reaping before releasing the
resident slot. Configuration replacement never mutates an existing peer. Sandbox
validates the selected confinement mode; Process owns pipes, termination and reaping.
The product supplies the dedicated journal directory independently of endpoint data.
Observed history and local settlement retain the journal's separate durability
contract; they are never promoted into native Agent Facts.

Preparation failure before any resource is acquired releases its resident slot.
An operation failure is distinct from a cleanup failure; only an unproven cleanup
keeps that slot reserved and makes provider retirement report failure.
