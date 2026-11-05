---
name: Apply ordinary approval policy to trusted application Tools
---

## Problem

The executor forced approval for every application Tool, and Kernel separately
required an approval receipt for its registration. Opening a Better Sidebar
document therefore stopped for consent even when the Turn and installed Tool
policies allowed it. These plugins already execute with operator authority at
activation. Per-call consent did not confine that process.

DSH's Tool pipeline defaults to allow and asks only when its ordered pre-execute
policy returns ask. It still runs guards before the body. That makes the policy
decision, rather than the Tool's package origin, determine the interaction.

## Decision

Use the Turn's existing approval requirement and ordinary Tool policy contributors
for application Tools. Report the application's actual unconfined execution mode
to policy and approval review. Keep its captured owner, model origin and exact
call proof; these identify the body and prevent replay, independently of approval.
The original application pipeline retains its own ask, deny and guard behavior.

## Alternatives considered

Keeping both the forced native approval and application ask repeats the user
interaction without adding confinement. Treating every plugin call as allowed
would instead ignore explicit Turn and native policy decisions. Reporting the
Turn's native process sandbox for a separately running plugin would misstate
the execution environment.

## Consequences

An ordinary allowed local plugin Tool runs without a new prompt. Explicit native
approval requirements and policy refusals still apply. A policy requiring native
approval and an application policy also returning ask may still cause two prompts;
combining those decisions needs the unfinished cross-process preparation seam.
This change does not grant sandbox enforcement to application code or introduce
a trusted-name allowlist. Durable formats and application protocols are unchanged.
