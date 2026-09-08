# Developing the terminal application

This tutorial takes you from a checkout to an isolated terminal session, a
focused change, and reproducible evidence. Run the shell examples from the
repository root in Bash on Linux or WSL2. Use a real terminal for interactive
commands. Native Windows input is unsupported; macOS terminal behavior needs
separate verification.

The [Terminal application contract](../crates/rsi/README.md#terminal-application)
owns user behavior and resource limits. The [Session contract](../crates/rsi/session-protocol/README.md)
owns Session operations. Workspace registration, Models and completed Process
output are injected independently into the terminal application. Use the [debugging reference](tui-debugging.md)
when a result differs from the expected behavior.

## 1. Establish a keyless baseline

Build with the repository toolchain, then run the TUI behavior tests:

```bash
cargo build --locked -p rsi
cargo test --locked -p rsi-terminal tui
```

These tests exercise input framing, editing, controller behavior, history
projection, selection, rendering, clipboard fixtures, and terminal cleanup.
They do not require a provider account. The
[Linux PTY tests](../crates/rsi/core/tests/service_host_cli/tui.rs) run the built
binary against a local HTTP provider fixture:

```bash
cargo test --locked -p rsi --test service_host_cli tui:: -- --test-threads=1
```

Use these fixtures when developing question, approval, cancellation, or
reconnection behavior. They provide a controlled provider response and isolate
Host state from your usual sessions.

## 2. Create an isolated interactive environment

Keep the configuration, Store, output cache, and workspace together so a
reproduction can be inspected after exit. Copy the built executable before
running it; concurrent Cargo commands can replace `target/debug/rsi`.

```bash
tui_dev=$(mktemp -d /var/tmp/rsi-tui-dev.XXXXXX)
tui_runtime=$(mktemp -d /tmp/rt.XXXXXX)
tui_rustup_home=${RUSTUP_HOME:-"$HOME/.rustup"}
mkdir -p "$tui_dev"/{home,config/rsi,state,cache,workspace/.cargo}
chmod 700 "$tui_runtime"
cp target/debug/rsi "$tui_dev/rsi"
chmod 700 "$tui_dev/rsi"
sha256sum "$tui_dev/rsi" > "$tui_dev/binary.sha256"

tui_rsi() {
  env HOME="$tui_dev/home" \
    XDG_CONFIG_HOME="$tui_dev/config" \
    XDG_STATE_HOME="$tui_dev/state" \
    XDG_CACHE_HOME="$tui_dev/cache" \
    XDG_RUNTIME_DIR="$tui_runtime" \
    RUSTUP_HOME="$tui_rustup_home" \
    CARGO_HOME="$tui_dev/workspace/.cargo" \
    "$tui_dev/rsi" "$@"
}
```

The fixture uses `/var/tmp` because the Linux sandbox gives tools a private
`/tmp`. Keep the copied executable visible to sandboxed tools: the binary also
serves as the apply-patch helper. See the
[Sandbox implementation](../crates/rsi-sandbox/local/src/lib.rs) and
[standard tool setup](../crates/rsi/core/src/main.rs).
The separate short runtime directory leaves room for the Host's state-root
digest and socket filename within the Unix socket path limit.

The environment overrides apply only to the child process. Preserving the
installed Rustup location lets sandboxed Rust commands find their toolchain
after HOME is isolated. Cargo's writable state remains inside the fixture
workspace. Other environment variables are inherited; remove unrelated
credentials from the launching shell when testing tools that inspect their
environment.

Create an Application Profile for the fullscreen client and another for
machine-readable inspection. Both use the same Host Profile:

```bash
mkdir -p "$tui_dev/config/rsi/application-profiles/"{dev-tui,dev-cli}
cat > "$tui_dev/config/rsi/application-profiles/dev-tui/application.profile.toml" <<'TOML'
format = 1
[[steps]]
kind = "plugin"
id = "connection"
plugin = "rsi.application.connection"
config = { host_profile = "dev" }
[[steps]]
kind = "plugin"
id = "application"
plugin = "rsi.application.tui"
TOML
cat > "$tui_dev/config/rsi/application-profiles/dev-cli/application.profile.toml" <<'TOML'
format = 1
[[steps]]
kind = "plugin"
id = "connection"
plugin = "rsi.application.connection"
config = { host_profile = "dev" }
[[steps]]
kind = "plugin"
id = "application"
plugin = "rsi.application.cli"
TOML
```

For an opt-in DeepSeek session, configure its declared route:

```bash
mkdir -p "$tui_dev/config/rsi/host-profiles/dev"
cat > "$tui_dev/config/rsi/host-profiles/dev/host.profile.toml" <<'TOML'
format = 1
[[steps]]
kind = "plugin"
id = "dev-deepseek"
plugin = "rsi.ai.provider.deepseek"
[steps.config]
deployment = "dev-deepseek"
endpoint = "https://api.deepseek.com"
credential = { owner = "rsi.ai.provider.deepseek", slot = "default" }
[steps.config.language_models.deepseek-chat]
context_window_tokens = 128000
default_output_reserve_tokens = 8192
max_output_reserve_tokens = 16384
TOML
cat > "$tui_dev/config/rsi/settings.json" <<'JSON'
{
  "rsi.agent": {
    "default_model": {"deployment": "dev-deepseek", "model": "deepseek-chat"},
    "turn_budget": {
      "maximum_elapsed_ms": 360000,
      "maximum_provider_attempts": 32,
      "maximum_tool_calls": 64,
      "maximum_generated_facts": 65536,
      "maximum_generated_fact_bytes": 67108864
    }
  }
}
JSON
```

These capacities are explicit development fixture settings, not measurements
of a provider's limits. Adjust the model and capacity declaration together for
your deployment. The [DeepSeek adapter source](../crates/rsi-ai/deepseek/src/lib.rs)
defines configuration and credential handling; the
[AI contract](../crates/rsi-ai/README.md) defines exact route selection.

Export `DEEPSEEK_API_KEY` through your local credential workflow before sending
a live task. The application does not automatically source `.local/dev/.env`.
Keep the key out of these TOML/JSON files, shell arguments, and captured output.
Submitting a task makes real API calls.

```bash
tui_rsi --profile dev-tui --cwd "$tui_dev/workspace" --session-id tui-dev
```

For repository work, populate this directory with a small fixture that has its
own Git baseline. Start with a source defect and a check that fails before the
repair. An empty directory inside an outer ignored repository gives misleading
Git evidence.

## 3. Reproduce the lifecycle you are changing

With no owner, the command above starts an embedded Host. Read the exit
consequence in the header before closing an active task. To reproduce a client
detach while execution continues, first exit the embedded client, then start
the isolated daemon:

```bash
tui_rsi host start --profile dev
tui_rsi --profile dev-tui --cwd "$tui_dev/workspace" --resume tui-dev
```

A fresh Session becomes durable after input acceptance. Resume a Session that
has accepted input; opening and closing an unused draft does not establish a
durable history. The line profile can inspect the same Session while a
compatible daemon owns it, or after the embedded client has exited:

```bash
tui_rsi --profile dev-cli --history tui-dev --output jsonl > "$tui_dev/history.jsonl"
tui_rsi host status
```

The history command returns a bounded page. Use the line client's history
commands for further pages, as described in its
[contract](../crates/rsi/README.md#line-application). Do not pipe the fullscreen
client into `tee`: its startup requires terminal stdin and stdout.

Stop the isolated daemon before replacing the copied binary or rebuilding its
Host Profile. The [Host lifecycle contract](../crates/rsi/service-host/README.md)
explains incompatible owner handling.

```bash
tui_rsi host stop
```

After inspection, remove only this fixture's `$tui_dev` and `$tui_runtime`
directories. Keep them while you still need the reproduction and its evidence.

## 4. Make a focused change

Follow the [application ownership boundary](architecture.md): terminal behavior
belongs to rsi-terminal; durable execution belongs to the Agent Kernel. Trace a
key or Fact through the [debugging reference](tui-debugging.md#trace-one-operation)
before changing its handling. Update the owning contract first when changing
observable behavior or a public API.

Add the regression at the narrowest meaningful boundary. A source selection
test should compare copied text and anchors after streaming or reflow. An
input test should feed bytes, including fragmented escape/paste sequences. A
resize test must resize an existing terminal backend and draw again; creating
a new backend at each size does not exercise resize handling.

Run the relevant test filter while editing, then the TUI group. For a Session
or model-routing change, also use the public-boundary tests listed in the
[debugging reference](tui-debugging.md#verify-the-public-boundary). Keep real
provider calls out of the default tests.

## 5. Inspect cells and a real terminal

Export deterministic cell grids without an interactive terminal:

```bash
tui_visual=$(mktemp -d /tmp/rsi-tui-visual.XXXXXX)
RSI_TUI_VISUAL_DIR="$tui_visual" cargo test --locked -p rsi-terminal \
  tui::render::tests::visual_scenes_are_bounded_and_exportable -- --exact
```

The test writes `scene-110x35.json`, `scene-80x24.json`, and `scene-42x12.json`.
Each contains `width`, `height`, and cells with coordinates, text, and colors.
It does not produce PNGs or approve new golden files. Inspect the cell grid for
layout and selection behavior, then capture the same interaction in a real
terminal or PTY for visual review.

Resize the same running client, open a menu, scroll a long reply, and verify
that the header, editor, and action hint stay visible. Try CJK text, combining
marks, emoji sequences, and tabs. Test color with NO_COLOR absent as well as
with it set; terminal font shaping and color policy are separate from the
stored cell evidence. Exercise ordinary exit and signal/cleanup paths in a
dedicated PTY, following the existing test fixture.

## 6. Record live evidence and finish

After keyless checks pass, use the isolated environment for an opt-in coding
task. Keep the executable fixed throughout coding and resume. Record its
digest, terminal size and environment, submitted task, durable outcome,
command exit/signal results, final diff, and independent verification output.

Keep the verifier outside the writable task workspace. Require evidence that
its assertions completed, such as an unpredictable completion marker, in
addition to exit status. A candidate can call `exit(0)` before assertions run.
Bound verifier runtime and output, and supervise descendant processes. A
passing oracle with a `budget_exceeded` Turn is different from a completed
coding workflow; retain both facts when reporting the result.

Resume the completed Session with the same binary and record that read-only
inspection adds no new Facts. Preserve failed runs instead of replacing them
with a later successful trace. Keep local credentials and run artifacts out of
tracked documentation; this tutorial does not depend on scripts under `.local/`.

Finish code changes with the relevant tests and checks:

```bash
cargo clippy --locked -p rsi -p rsi-session -p rsi-service-host --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo xtask verify-docs
```

Include affected AI packages when changing model contracts. Documentation-only
changes require the documentation gate. Run broader suites when the changed
surface or the task calls for them, and report only the platforms and live
integrations actually exercised.
