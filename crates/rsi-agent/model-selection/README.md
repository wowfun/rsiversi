# rsi-agent-model-selection

The ordinary Agent-only plugin owns durable Session model and reasoning effort
selection through a typed domain, Session command and projection. The initial
state is absent and falls back to the frozen Header baseline. Fork initialization
resets that domain; a child uses its own request-derived Header baseline.

Each Step captures one complete selection before preparing context. An explicit
whole-Turn override has priority, then the current domain, then the Header.
Retries reuse the Step capture. Ordinary UI inputs have no override, including
queued messages; Steer and Goal continuations use the next Step capture. A change
does not alter an in-flight provider call or an existing child Session.

The owning LanguageProfile supplies supported effort identifiers and defaults.
Selecting another model without an explicit effort requests that model's
default. Unsupported selections fail before provider I/O. Saving startup defaults
is an independent product operation with its own receipt.

The [decision](../../../.agents/notes/implemented/architecture/2026-09-14-session-model-selection.md)
records the alternatives and required verification.
