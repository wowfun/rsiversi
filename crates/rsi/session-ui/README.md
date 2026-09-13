# rsi-session-ui

This ordinary application contribution plugin owns Session and Tool inspection
cards over `rsi-ui`. It contributes an application-independent Session surface,
a Tool block renderer, and exact-source paging actions. Web and TUI render its
closed views and forward bound references without branching on its factory id.

`SessionUiTargetFactory` runs in the actual Shell surface and requires its
`SessionControllerContract`. Controller replacement therefore retires the UI
target through Meta's existing dependency graph. It publishes `UiTargetContract`
in the same isolated Local mapping. The application never supplies a Session id
to a contribution callback or creates another observer for a card.

The conversation renderer borrows shared `ToolState` and source membership.
Other blocks show a bounded text preview and their first 32 exact references;
the application source list remains the complete retained reference browser. It displays
intent completeness, phase and exact argument/result/rejection actions. Source
reads use the existing controller-owned exact Fact reader and closed `SourceRef`.
The plugin owns a 16 KiB raw UTF-8 page preference within the conversation window
ceiling; neither the card nor paging payload retains a Fact.

Completed `apply_patch` blocks additionally bind an inline Presentation. The
source captures only their exact ToolValue reference. Its owned asynchronous
model reads only the `evidence` subfield, bounded to 96 KiB pretty JSON, and
validates the version-1 32 KiB compact envelope before rendering recorded diffs.
Missing, oversized or unsupported evidence is explicit; it never triggers a
filesystem or git read. Omission is shown separately from an empty effect list.
The same model and standard renderer serve inline and detail presentations;
complete result paging remains available through the existing source action.

Goal and current-Turn Jobs are separate contributed surfaces. The target watches
the controller's existing projection cache and its one live Goal observer, then
invalidates only this target's presentations. Goal shows durable phase, allocated
rounds, model report and process-local driving separately. Create requires an
explicit positive round cap; Pause leaves claimed work running, Cancel targets
the automatic round, and Resume never resets allocations. Unknown controls keep
their exact identity and expose receipt checking without automatic resubmission.
Known control rejections remain visible across live and durable refreshes until
the next explicit control. Live Disarmed alone does not prove that the independent
durable projection has delivered the preceding round's settlement.
Jobs reads [finite current-Turn status pages](../session-protocol/README.md); controls offer refresh/paging,
never acquire, wait, read output, report or kill. Terminal diagnostic previews are
bounded and marked when shortened. Closed/finished scopes are shown unavailable.

Tool cards expose independently issued stdout/stderr references when the target
has the read-only Process output-cache capability. Output actions validate the
closed cache identity and decimal-string offset before I/O, capture that target's
reader and cancel on detail/target retirement. They read 16 KiB per page, within
the Process hard limit, and display safe text or exact hex with source byte
offsets. Pages never create a process, reopen a file or reconstruct missing
cache contents. A missing/evicted cache result remains an explicit read failure.
Text/hex toggling rereads the same immutable cache identity; provider withdrawal
can make a later page unavailable. Cache admission, retention and remote access
remain owned by the Process domain. Media byte access remains separate.

Read actions acquire the actual controller before I/O and cancel on detail close
or either owner retiring. An admitted mutation's ownership is independent of a
presentation waiter. Both adapters discard responses from closed detail or target
generations. Tests use deterministic Session fixtures; native/Worker and visual
product evidence are reported separately from live-provider evidence.

`SessionUiBinderFactory` is the explicit server export policy for `scope.kind =
session`. It accepts a validated Session id from an authenticated or trusted local
API origin, following the Session API's existing deployment-wide attachment
policy. It never accepts a raw Context, Local key, filesystem path or caller
origin in the scope. Revoked origins are rejected before creating a target.

Each export is a real child `ScopedProfile` with isolated controller, observation
sink and target slots. It retains the ordinary Session controller and its domain
facets; observation delivery invalidates only this binding's target through a
private weak registration lease, attached after target activation. Before that
activation no presentation exists; an early notice needs no retained replay.
The export sink releases its incoming projection clone; the controller retains
one shared latest snapshot for contribution models. Presentations never retain
a separate Session observer or Fact-page lease.
The UI API closes presentations and drains admitted actions before the binder
closes this Profile. A cancelled startup waiter leaves owned startup and cleanup
with the binder task tracker, including results which the waiter no longer receives.
Dropping a delivered binding also schedules owned Profile shutdown. Its binding
slot stays held until that shutdown finishes; a discarded response cannot leave
an orphan controller or release admission ahead of actual cleanup.

Each server target also publishes `UiBusinessApiContract` in its isolated mapping.
Its `SessionTargetClient` retains this binding's trusted origin and permits only
the controller's Session. Portable presentation children consume this explicit
facet; neither the UI source nor a presentation identity selects a global API.
