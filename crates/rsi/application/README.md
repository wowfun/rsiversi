# rsi-application

This library owns application entry and surface composition over ordinary Meta
plugins. It has no Session, Agent, transport, terminal or DOM dependency. An
ApplicationRun capability is the prepared application's single entry point;
its factory owns argument validation and its plugin generation owns runtime work.
The launcher selects and prepares a Profile, invokes that capability, then disposes
the composition. Help and invalid arguments must finish before backend activation.

RsiError owns the product entry failure classification (bootstrap exit 2,
execution exit 1). Native launchers and application parsers share the small
argument-reading helpers here; domain-specific grammars remain with their
applications. These helpers perform no I/O or application selection.

ScopedProfile mounts a frozen Host catalog and ordinary Profile below a supplied
Context. It prepares every initial leaf before creating its real meta-scope child
Fiber, isolates the catalog's Local markers and Profile control, and inherits
uncatalogued domain capabilities and Portable identities. Reload and shutdown
affect only that subtree. Profile status subscriptions report convergence and
retirement without granting parent shutdown authority. Preparation, activation failure and cleanup are reported
through their owning Meta/Profile errors. The caller owns the parent lifetime,
including cancellation during bootstrap; ScopedProfile never creates or shuts down
a Runtime. Native embedded service compositions and UI surfaces use this same seam.
Its read-only `inspect` observes only that scope's owning generation and descendants,
with no global resource counters; `profile_snapshot` and `profile_status` reuse the
same Profile control. Inspection of a retired scope is generation-fenced.

ShellFactory publishes a Session-free surface host. A frozen surface catalog is
injected by its constructor; its Profile configuration sets at most 16 concurrent
surfaces (8 by default). Every open operation owns one non-queued slot through
preparation, activation and complete cleanup. Dropping its waiter or Surface handle
requests disposal; Shell-owned tasks finish that cleanup. Shell withdrawal rejects
new opens, closes its surfaces and drains their work. Holding a Surface after Shell
withdrawal cannot preserve an active child generation. No second lifecycle engine,
surface Runtime or JavaScript composition owner is introduced.

Surface catalogs register controller/renderer markers for fresh Local isolation.
They deliberately inherit connection/domain markers from the Shell's parent
Context. Two surfaces can therefore share a connection without sharing their
selection or renderer state. A catalog without Session consumers remains usable
for unrelated capabilities.

Native tests and the [Worker probe](../../../fixtures/rsi/client-probe/README.md)
share public lifecycle scenarios. These exercise actual Profile/Meta ownership
with fixture leaf plugins; product UI rendering and live providers have separate
validation requirements.
