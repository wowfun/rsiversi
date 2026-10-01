# Pinned workbench comparison

`node fixtures/rsi/workbench-reference/verify.mjs --report ABSOLUTE_NEW_DIRECTORY`
builds two isolated Chromium scenes from the same explicit layout DTO. One imports
unmodified dockkit and primitive source from DeepSeek Harness revision
`4878cdabd87d4041bdaff61d04c966883b9fd07a`; the other imports RSI's vendored closure
and current document styles. The reference is not a screenshot of RSI itself.
Both use a fixed Linux DPR 1 canvas and identical content, with no masked pixels.
The fixture records computed component tokens and geometry, then rejects more
than 1% differing pixels at per-channel threshold 16. The source checkout is an explicit prerequisite (`RSI_DSH_REFERENCE`, default
`.references/rsi/deepseek-harness`); CI checks out the exact revision and runs both
this comparison and the Settings goldens. Chromium decodes the PNGs
for channel comparison. No upstream application is started.

This proves component rendering parity for that scene, not full-application
identity. Real paired RSI Settings, SSH and narrow layouts are checked separately
by the Web and native product fixtures. Geometry uses a 2 CSS px tolerance.

`node fixtures/rsi/workbench-reference/settings-goldens.mjs --report ABSOLUTE_NEW_DIRECTORY`
compares the actual Setup, Plugins, SSH and MCP components with reviewed RSI
goldens in four viewport sizes and both themes. The scene supplies fixed DTOs,
has no Service and confers no authority; real SSH and grant behavior belong to
the product fixtures. It uses no pixel masks and the same 16-channel / 1% bound.
The initial eight goldens were visually reviewed on Linux Chromium 153.0.8010.12:
800px desktop Settings, 188px navigation, scrollable options, narrow fullscreen
with top navigation, wrapped fingerprint and separate MCP field labels.
`--capture-candidates` only writes candidates into the report directory; it never
changes the goldens. Review intentional UI contract changes before replacing them.
