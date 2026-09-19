---
name: Discover selected workspace context without a Session trust flag
comment: Workspace selection supplies context sources without granting execution authority
---

## Problem

The immutable workspace trust flag made ordinary fresh Sessions omit project
instructions and skills. Directory-link support alone could not make the project's
`eli5` and `grilling` skills available. CLI, TUI, GUI drafts, forks and resumed
Sessions also carried a separate choice that users had to understand and preserve.

## Decision

The selected Session workspace participates in discovery by default. There is no
trust field, hidden default or opt-in parameter. Durable observation and failure handling are owned by the
[workspace observation decision](../architecture/2026-09-03-workspace-context-trust.md).
The [workspace-context contract](../../../../crates/rsi-agent/workspace-context/README.md)
owns discovery, invocation, precedence and read limits.

Project skill directories may link outside the repository. Directory handles pin
the selected target for an observation; skill files themselves remain regular
non-link files. Project `AGENTS.md` retains its separate project-directory boundary.
Fresh, resumed, forked and newly opened TUI Sessions follow the same rules.

Skill precedence is project sources (nearest directory first), then the RSI
configuration skill directory, then the personal `~/.agents/skills` directory.
First valid name wins consistently in discovery, preview and invocation. This
explicitly supersedes the user-first source policy in the
[earlier workspace decision](../architecture/2026-09-03-workspace-context-trust.md).
Project-specific workflows need to select the repository's matching skill without
renaming every shared command; the user chose that priority together with default
workspace discovery.

The Session format and Store schema reject older state without migration. The
explicit [reset option](../../../../crates/rsi/core/README.md) preserves a complete
Agent Store backup before creating empty history. Web draft upgrades remove only
the obsolete creation field; they preserve editing and pending request evidence
and do not replay requests.

## Alternatives considered

Keeping a flag defaulted to trusted would preserve a redundant wire and durable
field and retain inconsistent restore and draft-reuse behavior. Restoring the
opt-in gate would reintroduce the reported missing-skills behavior. Target
whitelists or link confirmation prompts would make one skill source's eligibility
depend on a second policy rather than the explicitly selected source.

User-first skill precedence would protect familiar personal names against
repository shadowing, as the earlier decision intended, but would silently suppress
a project's identically named workflow. That alternative is rejected for the
selected-workspace behavior; no target whitelist or confirmation gate replaces it.

## Consequences

Workspace selection accepts project-controlled model context, including subsequent
edits and external directory-link targets. Malicious instructions can influence
model behavior. A checkout can also shadow a familiar personal `/deploy` skill,
so selecting that name does not establish that its source is user-owned. Listing,
preview and invocation expose the same selected logical source. This is an accepted context-source risk, not a claim that sandbox
or Tool approval eliminates prompt injection. Tool invocation flags, Tool approval,
Sandbox policy, remote authentication and workspace registration remain independent.

Read failures preserve the complete previous baseline rather than accepting a
partial replacement. Because project sources can fail, they can delay refreshed
user context too; a bounded failure-episode diagnostic makes that delay visible.

Default verification uses temporary workspaces, HOME/XDG directories and offline
fixtures. Allowing selected project context does not authorize tests to read real
user state or contact live services. Current schemas and API versions belong to
their owning protocol contracts rather than this rationale.
