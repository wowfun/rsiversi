# RSI design system

RSI is a working conversation with an Agent. Reading an answer, understanding
ongoing work and deciding what to do next take precedence over application chrome.
Web and Linux Desktop share one document renderer. The terminal presents the same
actions and state through its own input and layout conventions.

## Information and feedback

Show information when it clarifies intent, enables an action or adds a distinct
fact. Keep the final answer visible. Present reasoning and Tools as concise
process summaries with deliberate expansion into source detail. Expansion
preserves the reader's position. Running, waiting for a person, successful,
failed and interrupted work have distinct text or symbols as well as color.
Unknown outcomes stay unknown until the owning service resolves them.

Primary content occupies the conversation. Navigation selects its Workspace and
Session; resources belong to the selected Session. A detail or modal layer owns
focus until dismissed, then restores focus to its surviving trigger or composer.
Hidden layers cannot receive submission or approval actions. Narrow layouts keep
the current task usable and expose secondary surfaces through explicit controls.

Actions use the same names throughout their lifecycle. Submission, selection and
successful edits show their resulting state rather than an extra success notice.
Errors retain the user's input and explain a possible next action. Passive reads
do not create progress notices. Approval and question responses remain bound to
the exact request and owner shown by the application.

## Input and execution

NextTurn queues ordinary input; Steer supplies an explicit steering intent.
The Rust controller owns action availability and labels. Renderer code forwards
explicit actions rather than choosing a delivery mode from a local busy flag.
The [client preferences contract](../client-preferences/README.md) owns GUI
submission keys and busy delivery preferences. GUI actions bind the acknowledged
presentation revision before preparation; a stale choice preserves the draft.
The [terminal reference](../../../apps/terminal/docs/tui-design.md) owns terminal key bindings.

GUI and TUI Stop target the exact Turn shown by the acknowledged presentation.
They preserve the draft and queued input. A stale target never cancels a later
Turn. Pending-only queue withdrawal is a separate operation. Line and ACP
cancellation keep their owning request-cancellation contracts.

## Visual roles

GUI uses system text fonts with CJK fallbacks and a separate code font. Body text
uses the bounded [Profile preference](../client-preferences/README.md). Light and dark palettes have
canvas, surface, primary and secondary text, focus, selection and outcome roles.
System mode follows the client's system appearance. Accent color identifies
actions and links; outcomes use their own semantic roles.

Component geometry follows role: details, compact and standard controls, code,
grouped content, cards and enclosing panels each have a consistent shape.
Interaction states retain their geometry. Focus is visible independently of
hover. Motion explains change and respects reduced-motion preferences.

The [Web document](../../../apps/web/README.md) owns exact GUI styles,
responsive behavior and presentation preferences. The terminal retains the
terminal's background and font, using spacing, rules and selection to express
hierarchy. Its [presentation library](../terminal-ui/README.md) owns semantic
cell styles. Protocol types remain independent of colors and layout.

## Authority

This reference owns shared presentation principles. Platform references own
their concrete layout and input. CSS and Rust style definitions own exact
values; prose does not maintain a second token catalog. Runtime states and
permissions remain with their subsystem and package contracts.

The design follows the local DeepSeek Harness reference's component roles and
the DESIGN.md reference's emphasis on explicit design intent. RSI retains its
own name and artwork. It uses system fonts and does not import brand fonts.
