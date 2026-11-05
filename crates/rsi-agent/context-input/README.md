# rsi-agent-context-input

The ordinary Agent-only plugin registers the draft-safe `context-input` command
(`rsi.context-input.command`). Its arguments contain an ordered `content` array of
native message blocks (text, image or verbatim file references) and
optional `attribution` with the caller's original `id` and JSON `source`. The
Session command argument contract bounds the complete object. It emits one
`Inject` input with native command provenance and no domain replacement. Text
block boundaries and attribution survive admission, consumption and replay.
The command resolves the current Media/File reader at invocation time and verifies
referenced objects before proposing publication. It retains references, never file
bodies or host paths. Receipted retries do not rerun this validation. Subsequent
typed publication and context projection do not reopen verbatim files. Frozen
conversation references remain exclusive to direct human input.

On a draft, the input stays unpublished until the first human submission. On a
published Session it enters the next Step without waking an idle Session. Native
command receipts own retry identity and publication; this command owns no queue
or scheduler. Callers submit a separate human `NextTurn` input when
they intend to ask a question. Attribution is presentation data, independent of
the owner-assigned native message ID and command provenance; it grants no
execution authority. Its bounds belong to the Session protocol.

The [decision](../../../.agents/notes/implemented/architecture/2026-11-03-context-input-commands.md)
explains why context injection needs no additional domain state.

The separate draft-safe `context-seed` command (`rsi.context-seed.command`)
replaces supplemental initial context in the `rsi.context-seed` v1 domain. Its
`text` argument is bounded to 64 KiB of UTF-8; an empty string clears it. This is
Session context, independent of pending human or injected messages, so clearing
the inbox does not remove it. It never wakes the Session. The next context
contribution atomically enters the text with ordinary plugin provenance and
clears the domain. Provider retries replay that committed input; later requests
retain it as history without entering it again. Forks reset the pending seed;
completed history already contains any consumed seed. Drafts retain it with their
ordinary unpublished lifetime; after publication native Store recovery owns it.
This contract carries reference text, not synthetic parent Facts, source roles,
or tool execution authority.
