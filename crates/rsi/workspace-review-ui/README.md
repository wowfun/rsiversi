# rsi-workspace-review-ui

This ordinary Application contribution renders the read-only workspace-review
API in the existing TUI, browser and desktop surface registry. Every action
resolves the actual surface Session controller and authorized Session header;
request payloads cannot substitute a workspace or conversation. Registry action
and view lifetimes fence reads. Opening the surface shows its scope summary
without calling the workspace-review API; once a presentation runs an inspection
action, target invalidations refresh that selected operation until the
presentation closes. No model call or execution starts on inspection.

The empty view reports when there are no changes available for review. Lists with recorded intervals
describe temporal comparisons, scope exclusions and counted omissions.
An unfinished review shows its status in the running Host. A completed capture
with observed changes supplies file counts and an end time.
The panel lists pending reviews and available comparisons in this Host. The
file-list action opens a finished review with recorded changes. Each
action reads at most one API page. Views retain at most 32 summaries, 16 file
names or 12 KiB of diff text;
continuation cursors advance only over displayed content. Diff text is plain code,
never HTML, and paths do not invoke a shell or external editor.
