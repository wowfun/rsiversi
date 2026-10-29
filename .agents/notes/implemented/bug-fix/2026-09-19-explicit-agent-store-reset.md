---
name: Keep Agent Store reset explicit and preserve failed startup evidence
---

## Problem

A Store reset moves the entire configured root. Treating every nonempty directory
as a Store could move unrelated configuration or credentials after a root typo.
Scanning all application arguments for the reset switch could also consume a
literal option value. Once a reset failed, marking the request consumed allowed
the same startup authority to retry as an ordinary open and conceal that failure.

## Decision

The launcher recognizes an application reset only before application arguments.
Management command grammars retain their explicit reset options. The
[launcher contract](../../../../crates/rsi/core/README.md) owns exact syntax and
startup ordering. The [SQLite contract](../../../../crates/rsi-agent/store-sqlite/README.md)
owns directory recognition, backup and writer-lock behavior.

Reset recognizes Store layout markers without opening or migrating the old
database. Unrelated nonempty directories are rejected before mutation. A missing
or empty root initializes directly. This recognition is a guard against mistaken
configuration, not proof that every file inside a configured root belongs to RSI.

The [unified launch decision](../architecture/2026-10-20-unified-product-launch.md)
supersedes activation-time reset injection with product-home maintenance before
startup. The SQLite factory exposes no reset request or one-shot notification
lifecycle. Standalone reset retains its success or failure receipt, including
the backup location after replacement initialization fails. Its caller owns any
explicit retry; ordinary activation cannot silently retry a reset.

## Alternatives considered

Matching HOME through ambient environment reads would couple the Store library
to launcher state and would miss other unrelated roots. Schema inspection before
backup would defeat recovery from unreadable or unsupported databases. Retrying
the reset implicitly could create multiple backups or hide an incomplete attempt.
Automatic backup pruning would violate the preservation promise.

## Consequences

Backup directories are never overwritten, rolled back or automatically deleted.
Backup and fresh initialization are not one crash-atomic filesystem transaction;
the preserved directory is the recovery artifact after a post-move failure.
Tests exercise this boundary under temporary roots, including failures before
and after backup, and never reset the operator's state.
