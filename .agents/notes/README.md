# Agent Notes

Agent Notes record decisions that affect this repository: the problem, the
chosen or proposed direction, genuine alternatives, and the trade-offs that
code and current-contract documentation cannot preserve. They are English-only
and are the sole home for durable design rationale.

## When to write one

Add or update an Agent Note in the same change when a maintainer may reasonably
revisit a decision about architecture, a public API or ABI, a wire, schema,
configuration, or durable format, a security boundary, repository process, or
testing strategy. A local implementation choice, mechanical rename, prose fix,
or ordinary bug fix does not require a note unless it changes one of those
decisions.

Update the active note that already owns the decision instead of creating
another one.

## Paths and lifecycle

Active notes use
`.agents/notes/{proposed|implemented|rejected}/{class}/YYYY-MM-DD-kebab-topic.md`.
The date is when the topic was first proposed and stays unchanged across moves.
The filename topic is a lowercase ASCII kebab slug and is independent from the
human-readable `name`.

- `proposed`: reviewed future work that is not fully shipped.
- `implemented`: the current decision, written in present tense and kept
  factually aligned with shipped paths, names, defaults, and mechanisms.
- `rejected`: a proposal retained while its reasoning prevents a plausible
  mistake.
- `archived`: a sealed implemented note whose rationale is no longer current
  guidance. Archived notes are history, not current authority.

Classes are closed and path-encoded:

- `feature`: a new user-facing capability.
- `bug-fix`: a defect whose resolution carries reusable decision rationale.
- `simplification`: removal of behavior, code, or surface area.
- `architecture`: structure and vocabulary of shipped source.
- `process`: tooling and workflow around the source.
- `testing`: test infrastructure or strategy.

## File format

Every note begins with YAML frontmatter and has no level-one Markdown heading:

```yaml
---
name: Human-readable title
comment: Optional nonempty single-line context
---
```

`name` is required and `comment` is optional. The lifecycle directory is the
sole status source. Unknown or duplicate fields, including a redundant
`status`, are invalid.

Canonical sections occur once, in the order shown, and contain nonempty prose.
Additional technical H2 sections may appear between canonical sections.

### Proposed

```markdown
## Problem
## Proposal
## Alternatives considered
## Acceptance criteria
## Risks
```

### Implemented

```markdown
## Problem
## Decision
## Alternatives considered
## Consequences
```

Implemented notes may not contain `## Proposal`, `## Plan`,
`## Migration plan`, or `## Acceptance criteria`.

### Rejected

```markdown
## Problem
## Proposal
## Alternatives considered
```

## Transitions and supersession

Moving proposed work to implemented keeps the filename date, rewrites the
proposal as present-tense shipped reality, and folds acceptance criteria and
risks into consequences or current verification. Moving it to rejected retains
the proposal under the rejected lifecycle directory.

Never rewrite an implemented note into the opposite decision. Record the new
decision in a new note and cross-link both. Delete a fully superseded active
note only after the current owner preserves its unique rationale, alternatives,
consequences, and verification; partial supersession keeps both notes.

## Archive

Only implemented notes may move to `archived/<class>/`. Move the file without
changing its frontmatter or body, repair inbound links, then append its seal:

```sh
cargo xtask verify-agent-notes --write
```

The versioned `archived/manifest.json` maps archive-relative note paths to
SHA-256 hashes. Git history is the only archive timestamp. Existing manifest
entries and sealed files are immutable and may never be replaced, edited,
moved, or deleted. Normal verification is read-only:

```sh
cargo xtask verify-agent-notes
```
