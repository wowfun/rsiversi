# rsi-service-ui

This ordinary native Application contribution bridges authenticated service UI
catalogs and standard declarative views into the existing local UI registry.
Each actual Session surface has one reader bound to its captured controller and
API connection. The document cannot supply another semantic scope. Catalog pages
have at most 64 entries; opening requires membership in the displayed page.

One remote observation and one validated model are retained per target. Opening
another surface, refreshing the catalog, explicit Close service view or retiring
the Session target releases the old observation. Closing a local detail may keep
this one cached view until replacement or target retirement. No background poller
or unbounded view map is created. Nonstandard renderer-only models are rejected.

Remote actions carry the exact displayed model revision and consume the server's
one-use ticket before invocation. Failure never retries an action. The local UI
registry also fences contribution/target retirement; the terminal's view revision
fences closed forms. Per-reader admission is nonqueued, and cancellation drains
through the existing UI action owners. This adapter starts no Session turn and
never forwards a caller-supplied workspace, Context or API authority.

Before invocation the reader consumes at most 16 already available observation
items. A changed model is shown with an explicit review notice; the old action
and its field values are not sent. A same-revision ticket refresh can be used
for that displayed model. Changes racing the request still fail without replay.

Proxy wrappers and the local Close control must fit the same UI view limits;
a remote view that leaves no room is rejected. Closing never forwards editable
remote fields. Presentation identities are fenced as well as model revisions.

Standard models preserve the remote model's bounded data and declared sources.
The bridge requires asynchronous presentation; synchronous view rendering cannot
preserve model sources or revision-bound image reads.
Source reads use the retained remote presentation and revision, validate declared
membership and forward at most 64 KiB through the same captured authenticated API.
A replaced or closed remote view cannot authorize reads from its old sources.
The relay waits up to 120 seconds for an admitted action's result, accommodating
the Browser owner's launch, initial navigation and retirement budgets without
replaying a mutation.
Remote action/observation state is serialized independently of the published
local model. Rendering and declared source reads use the last committed model
while an action is pending. A completed action publishes its view, data and exact
remote source revision together; source completion is rejected after replacement
or Close. If proxy expansion prevents publication, the remote observation is
released and a bounded error view replaces both action and source state. Target
retirement cancels admission and clears the published model.
