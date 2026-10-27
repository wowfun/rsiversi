#!/usr/bin/env bash
# Minimal installer. Application selection belongs to rsi.
set -euo pipefail
prefix="${HOME}/.local"
prefix_set=false
binary=''
uninstaller=''
path_update=true
progress() { printf '[rsi] %s\n' "$1" >&2; }
usage() {
    printf '%s\n' 'Usage: bash scripts/install.sh [--prefix DIR] [--binary FILE --uninstaller FILE] [--no-path]' \
        'Install rsi into DIR/bin with an independent uninstaller in DIR/libexec/rsi.' \
        'Use this script for the first installation; existing destinations are refused before building.' \
        'Without --binary, build the core from this checkout using cargo --release.' \
        'Add interfaces afterwards with rsi plugin install web or desktop.'
}
for argument in "$@"; do
    case "$argument" in --help|-h) usage; exit 0;; esac
done
while (($#)); do
    case "$1" in
        --prefix) (($# >= 2)) && [[ -n "$2" ]] || { usage >&2; exit 2; }; prefix=$2; prefix_set=true; shift 2;;
        --binary) (($# >= 2)) && [[ -n "$2" ]] || { usage >&2; exit 2; }; binary=$2; shift 2;;
        --uninstaller) (($# >= 2)) && [[ -n "$2" ]] || { usage >&2; exit 2; }; uninstaller=$2; shift 2;;
        --no-path) path_update=false; shift;;
        *) usage >&2; exit 2;;
    esac
done
case "$(uname -s)" in
    Linux*) ;;
    MINGW*|MSYS*)
        for tool in cygpath powershell.exe; do
            command -v "$tool" >/dev/null || { echo "Git Bash installation requires $tool." >&2; exit 2; }
        done
        script=$(cygpath -aw -- "$(dirname -- "${BASH_SOURCE[0]}")/install.ps1")
        arguments=(-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$script")
        if $prefix_set; then arguments+=(-Prefix "$(cygpath -aw -- "$prefix")"); fi
        if [[ -n "$binary" ]]; then arguments+=(-Binary "$(cygpath -aw -- "$binary")"); fi
        if [[ -n "$uninstaller" ]]; then arguments+=(-Uninstaller "$(cygpath -aw -- "$uninstaller")"); fi
        if ! $path_update; then arguments+=(-NoPath); fi
        progress 'Starting Windows installer...'
        MSYS2_ARG_CONV_EXCL='*' exec powershell.exe "${arguments[@]}"
        ;;
    *) echo 'Source installation requires Linux/WSL or Windows Git Bash/PowerShell.' >&2; exit 2;;
esac
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
[[ "$prefix" = /* ]] || prefix="$PWD/$prefix"
bin="${prefix%/}/bin"
destination="$bin/rsi"
check_destination() {
    if [[ ! -e "$destination" && ! -L "$destination" ]]; then return; fi
    if [[ -f "$destination" && ! -L "$destination" ]]; then
        progress "rsi is already installed at $destination"
        printf 'Upgrade this installation (Bash):\n  %q core upgrade --local %q\n' "$destination" 'ABSOLUTE_CORE_PACKAGE' >&2
        printf '%s\n' 'Replace ABSOLUTE_CORE_PACKAGE with a core package directory containing package.json, not a bare rsi executable.' \
            "Package preparation: $root/docs/user/source-install.md" >&2
    else
        progress "Installation destination is occupied by a directory or link: $destination"
    fi
    printf '%s\n' 'Choose another empty prefix with --prefix DIR.' >&2
    exit 2
}
control="${prefix%/}/libexec/rsi"
if [[ -f "$control/install.json" && ! -L "$control/install.json" ]] && head -c 65536 "$control/install.json" | grep -Eq '"phase"[[:space:]]*:[[:space:]]*"(uninstalling|installing)"'; then
    progress "Incomplete installation maintenance at $control/install.json. Finish uninstall before reinstalling."
    exit 2
fi
check_destination
for name in rsi-uninstall uninstall.sh install.json; do
    path="$control/$name"
    if [[ -e "$path" || -L "$path" ]]; then progress "Installation residue at $path. Finish uninstall before reinstalling, or choose another --prefix DIR."; exit 2; fi
done
if [[ -n "$binary" && -z "$uninstaller" || -z "$binary" && -n "$uninstaller" ]]; then progress '--binary and --uninstaller must be supplied together.'; exit 2; fi
for base in "$bin" "$control"; do
parent=$base
while :; do
    if [[ -L "$parent" ]]; then
        progress "Installation path traverses a link at $parent. Pass its resolved directory with --prefix DIR."
        exit 2
    fi
    if [[ -e "$parent" && ! -d "$parent" ]]; then
        progress "Installation parent is not a directory: $parent"
        printf '%s\n' 'Choose another empty prefix with --prefix DIR.' >&2
        exit 2
    fi
    [[ "$parent" != / ]] || break
    parent=$(dirname -- "$parent")
done
done
progress "Preparing installation in $bin..."
if [[ -z "$binary" ]]; then
    (
        cd -- "$root"
        progress 'Preparing release build...'
        build_env=("CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}" "RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}" "CI=true")
        while IFS= read -r declaration; do
            declaration=${declaration%$'\r'}
            [[ "$declaration" =~ ^([A-Za-z_][A-Za-z0-9_\(\)]*)(=([a-z0-9_-]+(/[a-z0-9_-]+)*))?$ ]] || { echo 'Invalid build environment declaration.' >&2; exit 2; }
            key=${BASH_REMATCH[1]}; suffix=${BASH_REMATCH[3]:-}
            if [[ -n "$suffix" ]]; then
                value="$root/target/rsi-build/$suffix"; mkdir -p -- "$value"
                build_env+=("$key=$value")
            elif IFS= read -r -d '' value < <(printenv -0 -- "$key"); then build_env+=("$key=$value"); fi
        done < "$root/apps/devtools/build-environment.txt"
        progress 'Building rsi (release); the first build may take several minutes...'
        build_started=$SECONDS
        env -i "${build_env[@]}" cargo build --locked --release -p rsi-cli -p rsi-uninstall
        progress "Build completed in $((SECONDS - build_started))s."
    )
    target="${CARGO_TARGET_DIR:-$root/target}"
    [[ "$target" = /* ]] || target="$root/$target"
    binary="$target/release/rsi"
    uninstaller="$target/release/rsi-uninstall"
else
    progress "Using existing binary: $binary"
fi
arguments=(install --interface-version 1 --prefix "$prefix" --binary "$binary" --uninstaller "$uninstaller" --launcher "$root/scripts/uninstall.sh")
if ! $path_update; then arguments+=(--no-path); progress 'Skipping PATH update (--no-path).'; fi
exec "$uninstaller" "${arguments[@]}"
