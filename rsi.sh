#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
cd -- "$root"
operation=${1:-}; if (($#)); then shift; fi
windows=false
case "$(uname -s)" in MINGW*|MSYS*) windows=true;; Linux*) ;; *) echo 'Source entry supports Linux/WSL and Windows Git Bash.' >&2; exit 1;; esac
fail() { printf '%s\n' "$*" >&2; exit 1; }
bundle() {
    local record="$root/target/rsi-app/current" id
    [[ -f "$record" && ! -L "$record" ]] || fail 'No publication. Run setup, then build.'
    # Application publication grammar: apps/devtools/README.md.
    IFS= read -r -n 22 id < "$record" || fail 'Invalid publication record'
    [[ "$id" =~ ^[0-9]{20}$ && $(wc -c < "$record") -eq 21 ]] || fail 'Invalid publication identifier'
    local path="$root/target/rsi-app/generations/$id/bundle" suffix=''
    $windows && suffix=.exe
    [[ -f "$path/rsi$suffix" && -f "$path/receipt.json" && -f "$path/.rsi-generation.lock" ]] || fail 'Incomplete publication. Run build.'
    printf '%s\n' "$path"
}
case "$operation" in
    path) (($#==0)) || fail 'path takes no arguments'; bundle; exit;;
    web|tui)
        if $windows && [[ "$operation" == tui && -z "${WT_SESSION:-}" ]]; then fail 'Use Git Bash inside Windows Terminal for TUI. mintty is unsupported.'; fi
        current=$(bundle); suffix=''; $windows && suffix=.exe
        if $windows; then export MSYS2_ARG_CONV_EXCL='*'; fi
        exec "$current/rsi$suffix" "$operation" "$@";;
    doctor|setup|build) ;;
    *) fail 'Usage: ./rsi.sh doctor|setup|build [--debug]|web [ARGS]|tui [ARGS]|path';;
esac
if $windows; then
    # Setup/build flags contain no opaque program arguments; launch above preserves argv.
    export MSYS2_ARG_CONV_EXCL='*'
    exec powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$root/rsi.ps1")" "$operation" "$@"
fi
[[ "$operation" == build || $# -eq 0 ]] || fail "$operation takes no arguments"
missing=0
for command in git rustup node cc; do
    if ! command -v "$command" >/dev/null 2>&1; then echo "Missing $command; see the source-install guide." >&2; missing=1; fi
done
if command -v git >/dev/null && ! git rev-parse --show-toplevel >/dev/null 2>&1; then echo 'A Git clone is required; source ZIP archives are unsupported.' >&2; missing=1; fi
((missing==0)) || exit 1
[[ $(node -p 'process.platform') == linux ]] || fail 'WSL requires Linux Node, not a Windows executable inherited through PATH.'
rust=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([0-9.]*\)".*/\1/p' rust-toolchain.toml)
[[ "$rust" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'Invalid repository Rust toolchain version declaration'
bindgen=$(node -e 'const s=require("fs").readFileSync("Cargo.lock","utf8"); const m=s.match(/\[\[package\]\]\s*name = "wasm-bindgen"\s*version = "([0-9.]+)"/); if(!m) { console.error("Invalid repository wasm-bindgen version declaration"); process.exit(1) } console.log(m[1])')
node -e 'const p=require("./apps/web/package.json"); const actual=process.versions.node.split(".").map(Number), min=p.engines.node.slice(2).split(".").map(Number); if(actual[0]<min[0] || actual[0]===min[0] && actual[1]<min[1]) { console.error("Node "+p.engines.node+" required"); process.exit(1) }'
case "$(uname -m)" in x86_64) helper=x86_64-unknown-linux-musl;; aarch64) helper=aarch64-unknown-linux-musl;; *) fail 'Unsupported Linux CPU';; esac
command -v "${RSI_MUSL_CC:-musl-gcc}" >/dev/null || fail 'Install musl-tools or set an absolute RSI_MUSL_CC.'
tools="$root/target/rsi-tools"
if [[ "$operation" == setup ]]; then
    rustup toolchain install "$rust" --profile minimal
    rustup target add --toolchain "$rust" wasm32-unknown-unknown "$helper"
    mkdir -p "$tools/pnpm"
    cp apps/devtools/tools/package{,-lock}.json "$tools/pnpm/"
    npm ci --prefix "$tools/pnpm" --ignore-scripts --no-audit --no-fund
    if [[ ! -x "$tools/wasm-bindgen/bin/wasm-bindgen" ]] || [[ $("$tools/wasm-bindgen/bin/wasm-bindgen" --version) != "wasm-bindgen $bindgen" ]]; then
        cargo install --locked --root "$tools/wasm-bindgen" --version "$bindgen" wasm-bindgen-cli
    fi
    echo 'Setup complete. Next: ./rsi.sh build'
else
    installed_targets=$(rustup target list --installed --toolchain "$rust" 2>/dev/null) || fail "Cannot inspect pinned Rust $rust; run setup."
    grep -qx wasm32-unknown-unknown <<< "$installed_targets" || fail 'WASM target missing; run setup.'
    grep -qx "$helper" <<< "$installed_targets" || fail 'Linux musl target missing; run setup.'
    [[ -f "$tools/pnpm/node_modules/pnpm/bin/pnpm.mjs" && -x "$tools/wasm-bindgen/bin/wasm-bindgen" ]] || fail 'Repository tools missing; run setup.'
    if [[ "$operation" == doctor ]]; then echo 'Source prerequisites are ready.';
    else exec cargo run --locked -p rsi-app-tools -- dist web "$@"; fi
fi
