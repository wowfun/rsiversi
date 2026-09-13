---
name: Session API Goal acceptance with an external coding oracle
---

## Problem

The terminal coding evaluator supervises an isolated terminal application and
grades allowlisted source through an external immutable oracle. It does not
exercise Session API Goal control, and the desktop live smoke proves only one
tool-created file. Neither establishes autonomous continuation or semantic
compaction merely by exiting successfully.

## Decision

A Linux evaluation executable supports the standard patch helper, starts the
real standard Service daemon, connects through its local API, and controls a
bounded Goal using the public Session handle. The Python runner reuses the
coding tasks, source admission, oracle, namespace construction and descendant
supervision of the terminal evaluator. Request identities, budgets, model,
attempt count and evidence output are fixed before each run. Process restart
reattaches disarmed and requires an explicit control; reads cannot resume work.

Reports preserve API receipts, canonical Goal/Turn outcomes, durable traces,
compaction attempts and Finished candidates, budget use and oracle results.
Finished event counts are not installed-summary counts: source selection and
policy validation still govern whether each candidate is usable. An
authenticated model completion claim does not substitute for oracle success. A
zero process exit without terminal evidence is an infrastructure failure. Goal
budget/failure dispositions remain failures even if edited source happens to
pass an oracle. Original failed attempts remain available.

## Alternatives considered

Driving slash commands through the terminal would preserve the wrong public
surface. Reimplementing grading inside the Agent would expose the oracle to
editable code. A GUI-only long-task runner would mix rendering and execution
failure domains and make bounded continuation faults harder to isolate.

## Consequences

API setup/control/read calls use infrastructure deadlines independent of Goal
round waiting. Final grading is bounded by the task's remaining allowance and
cannot convert expiry into a pass. Cleanup and evidence retention may finish
later so a classified failure remains inspectable. CI separates oracle/API
evaluation from unit/TUI execution, including its own Python unit checks, and
attempts artifact upload even after failure. Unstarted scenarios cannot provide
product evidence; admitted-task exceptions retain a redacted classified report.

The [evaluation contract](../../../../crates/rsi/core/eval/README.md) owns the
commands, frozen limits, isolation and report classification.

Deterministic API scenarios exercise continuation and restart without live
credentials. External-oracle self-tests retain their compile/run isolation and
descendant timeout proofs. Fixed DeepSeek attempts use only the authorized key
assignment in an isolated environment. Browser/desktop mechanism tests and live
GUI smoke retain independent reports and actual visual evidence.

Small coding tasks may never create context pressure. Their live pass
establishes only those fixed tasks and cannot establish live compaction, general
coding ability or native Windows/macOS support. Hard process interruption can
leave an admitted attempt without terminal evidence; preserve that incomplete
attempt and identify any later diagnostic attempt separately.

Scripted pressure adds an applicable Usage signal and a large old interaction.
Each task stage matches its ordered scripted exchanges to durable model intents,
the exact summary output and successful Finished, then requires the complete
framed summary in the next ordinary request with the selected large answer
removed. Plan/source and later request hashes are retained. This is compatible
Chat Completions mechanism evidence; it does not exercise live DeepSeek Responses
compaction or a fork. Event counts alone cannot establish summary installation.

The scripted provider also invokes the actual Bash Tool to attempt configuration
and SQLite overwrites while checking credential removal and workspace writes.
Canonical results and enforcement stamps establish that restricted Tool path;
the outer daemon must retain write access to its state. Hashing the configured
files before and after catches configuration drift without making the database
read-only. Incomplete descendant cleanup invalidates a run before completion or
timeout classification.
