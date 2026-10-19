#!/usr/bin/env bash
set -euo pipefail
operation=${1:-}; if (($#)); then shift; fi
help() {
    case "${1:-}" in
        doctor|setup) printf '%s\n' \
            "Usage: bash scripts/rsiversi.sh $1 [tui|web|desktop] [--with-ssh-helper] [-h|--help]" \
            'Default target: tui (Rust/native compiler only; no Node, WASM or musl).' \
            'doctor checks prerequisites. setup prepares pinned Rust and target-specific local tools.' \
            'Web/Desktop add Node, pnpm and wasm-bindgen; Linux Desktop also needs GTK/WebKit development libraries.' \
            'Linux Web/Desktop include the musl SSH helper; TUI opts in with --with-ssh-helper.' \
            'Install system prerequisites separately; setup does not change machine security settings.';;
        build) printf '%s\n' \
            'Usage: bash scripts/rsiversi.sh build [tui|web|desktop] [ABSOLUTE_OUTPUT] [--debug] [--with-ssh-helper] [--verbose] [-h|--help]' \
            'Default target: tui. Default profile: release; --debug selects debug.' \
            'TUI includes native CLI, Headless, ACP, HTTP API and management. Web adds assets/Worker; Desktop adds the window application.' \
            'Linux Web/Desktop include the SSH helper (always release); TUI opts in with --with-ssh-helper. Windows rejects this flag.' \
            'Without ABSOLUTE_OUTPUT, publish under target/rsi-app/TARGET; each target has a separate build cache.' \
            'ABSOLUTE_OUTPUT must be a new absolute directory outside target/rsi-app.' \
            'Linux/WSL: /absolute/path. Git Bash also accepts Windows drive/UNC paths and converts POSIX paths.' \
            'Progress and diagnostics go to stderr; stdout contains the result JSON. --verbose mirrors the full build log.' \
            'Example: bash scripts/rsiversi.sh build desktop --debug';;
        path) printf '%s\n' \
            'Usage: bash scripts/rsiversi.sh path [tui|web|desktop] [-h|--help]' \
            'Print the selected current managed bundle directory (default: tui).' \
            'Requires a successful managed build; never invokes build tools or falls back to another target.';;
        *) printf '%s\n' \
            'RSIversi source entrypoint' \
            'Usage: bash scripts/rsiversi.sh COMMAND [ARGS]' \
            '' \
            'Commands:' \
            '  doctor [TARGET]         Check target-specific source-build prerequisites.' \
            '  setup [TARGET]          Prepare target-specific pinned tools.' \
            '  build [TARGET] [OUTPUT] Build the selected application (release by default; --debug for debug).' \
            '  path [TARGET]           Print its current managed bundle directory.' \
            '  tui [--from TARGET] [ARGS] Start TUI; TARGET may be tui, web or desktop.' \
            '  web [--from TARGET] [ARGS] Start Web; TARGET may be web or desktop.' \
            '  desktop [ARGS]          Start the Desktop application.' \
            '' \
            'TARGET defaults to tui for management commands. Launch defaults to its own target.' \
            'Use --from web/desktop to run clients from the same build family and share a Linux Service Host.' \
            'Only a leading --from is consumed; an optional -- ends script option parsing.' \
            'OUTPUT is a new absolute directory outside target/rsi-app; omit it for a managed build.' \
            'Use COMMAND --help for management help. web/tui/desktop help is forwarded to the application.' \
            'Launch and path never build or select another publication automatically.' \
            'Supports Linux/WSL and Windows Git Bash. Windows requires Windows 11 24H2+ x64.' \
            'Windows TUI requires Windows Terminal; Desktop requires WebView2 at launch.' \
            'The repository is located from this script, not the current working directory.' \
            '' \
            'Example: bash scripts/rsiversi.sh setup desktop' \
            '         bash scripts/rsiversi.sh build desktop --verbose' \
            '         bash scripts/rsiversi.sh desktop' \
            'Guide: docs/user/source-install.md';;
    esac
}
case "$operation" in
    ''|-h|--help) help; exit 0;;
    doctor|setup|build|path)
        for argument in "$@"; do
            case "$argument" in -h|--help) help "$operation"; exit 0;; esac
        done;;
    web|tui|desktop) ;;
    *) printf 'Unknown command: %s. Run bash scripts/rsiversi.sh --help.\n' "$operation" >&2; exit 1;;
esac
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
windows=false
case "$(uname -s)" in MINGW*|MSYS*) windows=true;; Linux*) ;; *) echo 'Source entry supports Linux/WSL and Windows Git Bash.' >&2; exit 1;; esac
fail() { printf '%s\n' "$*" >&2; exit 1; }
bundle() {
    local target=$1 record="$root/target/rsi-app/$1/current" id
    [[ -f "$record" && ! -L "$record" ]] || fail "No $target publication. Run scripts/rsiversi.sh setup $target, then build $target."
    # Application publication grammar: apps/devtools/README.md.
    IFS= read -r -n 22 id < "$record" || fail 'Invalid publication record'
    [[ "$id" =~ ^[0-9]{20}$ && $(wc -c < "$record") -eq 21 ]] || fail 'Invalid publication identifier'
    local path="$root/target/rsi-app/$target/generations/$id/bundle" suffix=''
    $windows && suffix=.exe
    [[ -f "$path/rsi$suffix" && -f "$path/receipt.json" && -f "$path/.rsi-generation.lock" ]] || fail "Incomplete $target publication. Run scripts/rsiversi.sh build $target."
    if [[ "$target" == desktop ]]; then [[ -f "$path/rsi-desktop$suffix" ]] || fail 'Desktop executable missing; run build desktop.'; fi
    printf '%s\n' "$path"
}
case "$operation" in
    web|tui|desktop)
        target=$operation
        if [[ "$operation" != desktop && "${1:-}" == --from ]]; then
            (($#>=2)) || fail '--from requires a target'
            target=$2; shift 2
            case "$target" in tui|web|desktop) ;; *) fail 'Invalid --from target';; esac
            [[ "$operation:$target" != web:tui ]] || fail 'Web requires --from web or --from desktop'
        fi
        if [[ "${1:-}" == -- ]]; then shift; fi
        if $windows && [[ "$operation" == tui && -z "${WT_SESSION:-}" ]]; then fail 'Use Git Bash inside Windows Terminal for TUI. mintty is unsupported.'; fi
        current=$(bundle "$target"); suffix=''; $windows && suffix=.exe
        if $windows; then export MSYS2_ARG_CONV_EXCL='*'; fi
        if [[ "$operation" == desktop ]]; then exec "$current/rsi-desktop$suffix" "$@"; fi
        exec "$current/rsi$suffix" "$operation" "$@";;
esac
cd -- "$root"
target=tui
case "${1:-}" in tui|web|desktop) target=$1; shift;; esac
if [[ "$operation" == path ]]; then (($#==0)) || fail 'path accepts one target'; bundle "$target"; exit; fi
# Validate before prerequisites. The Rust producer independently validates this boundary.
helper=false; debug=false; verbose=false; output=''; forward=()
for argument in "$@"; do
    case "$argument" in
        --with-ssh-helper) $helper && fail 'Duplicate --with-ssh-helper'; helper=true;;
        --debug) [[ "$operation" == build ]] && ! $debug || fail "Invalid or duplicate argument: $argument"; debug=true;;
        --verbose) [[ "$operation" == build ]] && ! $verbose || fail "Invalid or duplicate argument: $argument"; verbose=true;;
        *)
            [[ "$operation" == build && -z "$output" ]] || fail "Invalid argument: $argument. Use $operation --help."
            if $windows; then
                if [[ "$argument" == /* ]]; then argument=$(cygpath -w -- "$argument");
                else [[ "$argument" =~ ^[A-Za-z]:[/\\] || "$argument" == \\\\* ]] || fail "Invalid argument: $argument. Use $operation --help."; fi
            else [[ "$argument" == /* ]] || fail "Invalid argument: $argument. Use $operation --help."; fi
            output=$argument;;
    esac
    forward+=("$argument")
done
if $windows; then
    $helper && fail 'SSH helper publication requires Linux; omit --with-ssh-helper on Windows'
    export MSYS2_ARG_CONV_EXCL='*'
    exec powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$root/scripts/rsiversi.ps1")" "$operation" "$target" "${forward[@]}"
fi
web=false; [[ "$target" != tui ]] && web=true
$web && helper=true
missing=0
required=(git rustup cc); $web && required+=(node npm)
for command in "${required[@]}"; do
    if ! command -v "$command" >/dev/null 2>&1; then echo "Missing $command; see the source-install guide." >&2; missing=1; fi
done
if command -v git >/dev/null && ! git rev-parse --show-toplevel >/dev/null 2>&1; then echo 'A Git clone is required; source ZIP archives are unsupported.' >&2; missing=1; fi
((missing==0)) || exit 1
rust=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([0-9.]*\)".*/\1/p' rust-toolchain.toml)
[[ "$rust" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'Invalid repository Rust toolchain version declaration'
if $web; then
[[ $(node -p 'process.platform') == linux ]] || fail 'WSL requires Linux Node, not a Windows executable inherited through PATH.'
bindgen=$(node -e 'const s=require("fs").readFileSync("Cargo.lock","utf8"); const m=s.match(/\[\[package\]\]\s*name = "wasm-bindgen"\s*version = "([0-9.]+)"/); if(!m) { console.error("Invalid repository wasm-bindgen version declaration"); process.exit(1) } console.log(m[1])')
node -e 'const p=require("./apps/web/package.json"); const actual=process.versions.node.split(".").map(Number), min=p.engines.node.slice(2).split(".").map(Number); if(actual[0]<min[0] || actual[0]===min[0] && actual[1]<min[1]) { console.error("Node "+p.engines.node+" required"); process.exit(1) }'
fi
if [[ "$target" == desktop ]]; then
    command -v pkg-config >/dev/null && pkg-config --exists gtk+-3.0 webkit2gtk-4.1 || fail 'Install GTK 3 and WebKitGTK 4.1 development libraries for Desktop.'
fi
if $helper; then
[[ -z "${RSI_MUSL_CC+x}" || "$RSI_MUSL_CC" == /* ]] || fail 'RSI_MUSL_CC must be absolute'
case "$(uname -m)" in x86_64) helper_target=x86_64-unknown-linux-musl;; aarch64) helper_target=aarch64-unknown-linux-musl;; *) fail 'Unsupported Linux CPU';; esac
command -v "${RSI_MUSL_CC:-musl-gcc}" >/dev/null || fail 'Install musl-tools or set an absolute RSI_MUSL_CC.'
fi
tools="$root/target/rsi-tools"
if [[ "$operation" == setup ]]; then
    rustup toolchain install "$rust" --profile minimal
    if $helper; then rustup target add --toolchain "$rust" "$helper_target"; fi
    if $web; then
    rustup target add --toolchain "$rust" wasm32-unknown-unknown
    mkdir -p "$tools/pnpm"
    cp apps/devtools/tools/package{,-lock}.json "$tools/pnpm/"
    npm ci --prefix "$tools/pnpm" --ignore-scripts --no-audit --no-fund
    if [[ ! -x "$tools/wasm-bindgen/bin/wasm-bindgen" ]] || [[ $("$tools/wasm-bindgen/bin/wasm-bindgen" --version) != "wasm-bindgen $bindgen" ]]; then
        cargo install --locked --root "$tools/wasm-bindgen" --version "$bindgen" wasm-bindgen-cli
    fi
    fi
    echo "Setup complete. Next: ./scripts/rsiversi.sh build $target"
else
    setup="bash scripts/rsiversi.sh setup $target"; $helper && [[ "$target" == tui ]] && setup+=' --with-ssh-helper'
    installed_targets=$(rustup target list --installed --toolchain "$rust" 2>/dev/null) || fail "Cannot inspect pinned Rust $rust; run $setup."
    if $web; then
    grep -qx wasm32-unknown-unknown <<< "$installed_targets" || fail "WASM target missing; run $setup."
    [[ -f "$tools/pnpm/node_modules/pnpm/bin/pnpm.mjs" && -x "$tools/wasm-bindgen/bin/wasm-bindgen" ]] || fail "Repository tools missing; run $setup."
    fi
    if $helper; then grep -qx "$helper_target" <<< "$installed_targets" || fail "Linux musl target missing; run $setup."; fi
    if [[ "$operation" == doctor ]]; then echo 'Source prerequisites are ready.';
    else exec cargo run --locked -p rsi-app-tools -- dist "$target" "$@"; fi
fi
