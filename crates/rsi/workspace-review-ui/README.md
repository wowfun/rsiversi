# rsi-workspace-review-ui

This ordinary Application contribution renders the read-only workspace-review
API in the existing TUI, browser and desktop surface registry. Every action
resolves the actual surface Session controller and authorized Session header;
request payloads cannot substitute a workspace or conversation. Registry action
and view lifetimes fence reads. No model call or execution starts on inspection.

The empty view explains when a review begins. Lists with recorded intervals
describe temporal comparisons, scope exclusions and counted omissions.
An unfinished review shows its status until capture supplies file counts and an
end time. A different owner epoch marks an unfinished summary interrupted.
Finished summaries retain their counts after restart. The file-list action
requires a finished review with recorded changes in the current epoch. Each
action reads at most one API page. Views retain at most 32 summaries, 16 file
names or 12 KiB of diff text;
continuation cursors advance only over displayed content. Diff text is plain code,
never HTML, and paths do not invoke a shell or external editor.
