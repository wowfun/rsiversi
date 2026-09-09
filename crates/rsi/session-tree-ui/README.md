# rsi-session-tree-ui

This ordinary UI contribution owns the read-only Agent tree inspector shared by
Web and TUI. Its `rsi.session.tree.ui` factory registers a surface and closed read
actions with the existing UI registry. The actual surface's SessionController
supplies the root identity; its composed Session capability supplies finite
inspection, Header and history reads. No child controller, observer, Shell surface,
execution capability or mutable Session action is created.

An ordinary target factory in each actual surface captures its exact controller
and Session service through declared Local dependencies. It publishes one finite
reader with a process/Worker-unique revision. Every button carries that revision;
replacing only this reader cannot retarget an old action even while the broader UI
target reference remains current. Reader retirement cancels already admitted reads.

Each action checks selection against a freshly captured atomic root subtree.
Session identifiers in button payloads are locators, never permission grants.
Only the current root and its recorded descendants are selectable. Direct children
are paged in durable Session order, at most 32 rows; breadcrumbs follow recorded
parent links. A textual breadcrumb accompanies the navigation actions so the TUI shows the
selected path with its action menu closed. Activity labels report the actual
open-Turn, activation and queued work flags. Idle does not assert successful
completion. Membership and activity
are snapshots; explicit refresh reads them again.

History reads at most 64 Facts before a captured watermark. Earlier-page actions
retain that watermark; Latest captures a new one. The inspector displays bounded
semantic previews, including shared Tool outcome classification, and lets a user
open exact Fact fields. Each record preview has 512 UTF-8 bytes; exact fields use
16 KiB windows with explicit truncation and paging, and field choices use 32-row
pages. Sequence and byte cursors travel as decimal
strings, remain within the selected watermark, and never carry complete Facts.
Page display retains only its closed view; the owning Session/API history budget
still permits larger transient valid Facts. These presentation bounds are not RSS
limits.

Every read remains tracked by its original UI action owner. Target/contribution
retirement and closing/replacing the inspector cancel its local future and reject
late presentation; these operations never cancel Agent work. Web uses its one
existing detail slot and TUI uses its existing contributed-card menu and detail
renderer. Native and Worker product tests distinguish real transport/visual
evidence from deterministic direct-capability fixtures.
