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
ceiling; neither the card nor paging payload retains a Fact. Output cache reading
and Media byte access remain with their existing separate domain contracts.

Read actions acquire the actual controller before I/O and cancel on detail close
or either owner retiring. An admitted mutation's ownership is independent of a
presentation waiter. Both adapters discard responses from closed detail or target
generations. Tests use deterministic Session fixtures; native/Worker and visual
product evidence are reported separately from live-provider evidence.
