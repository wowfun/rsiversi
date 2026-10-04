# Workflow tutorial

Configure the shipped, disabled `program-runtime` leaf in a writable Host Profile:
set its complete JSON configuration to `{"node":"/absolute/path/to/node"}` and
then enable it through the existing Plugins leaf preview/commit controls. Select
that same Host in every application. Saving the RestartRequired leaf does not
activate it; explicitly restart the Host. Check Workflows readiness and run a
small approved Workflow to verify Node execution. A configuration read does not
spawn Node. The [product contract](../../../crates/rsi/core/README.md) owns Profile
selection; the [Program contract](../../../crates/rsi-agent/program/README.md)
owns runtime configuration and authority.

Choose the `workflow` Agent preset and set Agent Settings sandbox to `read-only`
before creating this tutorial's Session. Copy one sample directory from `skills/`
into the tutorial workspace's `.agents/skills/`. Do not install these examples in
the repository's global Skill roots. Create left.json with `{"n":19}`, right.json
with `{"n":23}`, or review.txt with your review material. Ask the agent to discover
and read the selected Skill, then execute it. Explicit `$workflow-collect` or
`$workflow-review` invocation also uses the existing Skill machinery.

Open Workflows in Resources (or `/workflows` in TUI). Inspect the canonical state,
child receipts, frozen script and paged result. Cancel workflow requests cancellation;
terminal Cancelled proves owned cleanup finished. Host restart interrupts, without
replaying scripts. The [Session contract](../../../crates/rsi/session/README.md)
owns history, cancellation and orphan behavior.

The opt-in browser fixture uses `RSI_TEST_NODE=/absolute/node RSI_WEB_REPORT=/new/report
node fixtures/rsi/web-product/workflow-verify.mjs` after a paired Web build.
Real model behavior is checked separately with the existing workflow-live fixture:
supply an authorized `RSI_LIVE_ENV_FILE`, `RSI_LIVE_MODEL`, `RSI_TEST_NODE`, a new
`RSI_WEB_REPORT`, and `RSI_WORKFLOW_SKILL=workflow-collect`. Default tests use a
keyless loopback provider. Live failures and source mismatches must remain visible.
