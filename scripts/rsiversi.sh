#!/usr/bin/env bash
# Minimal installer. Application selection belongs to rsi.
set -euo pipefail
prefix="${HOME}/.local"
prefix_set=false
binary=''
path_update=true
progress() { printf '[rsi] %s\n' "$1" >&2; }
usage() {
    printf '%s\n' 'Usage: bash scripts/rsiversi.sh [--prefix DIR] [--binary FILE] [--no-path]' \
        'Install only rsi into DIR/bin and add it to the user PATH.' \
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
        script=$(cygpath -aw -- "$(dirname -- "${BASH_SOURCE[0]}")/rsiversi.ps1")
        arguments=(-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$script")
        if $prefix_set; then arguments+=(-Prefix "$(cygpath -aw -- "$prefix")"); fi
        if [[ -n "$binary" ]]; then arguments+=(-Binary "$(cygpath -aw -- "$binary")"); fi
        if ! $path_update; then arguments+=(-NoPath); fi
        progress 'Starting Windows installer...'
        MSYS2_ARG_CONV_EXCL='*' exec powershell.exe "${arguments[@]}"
        ;;
    *) echo 'Source installation requires Linux/WSL or Windows Git Bash/PowerShell.' >&2; exit 2;;
esac
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
progress "Preparing installation in $prefix/bin..."
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
        env -i "${build_env[@]}" cargo build --locked --release -p rsi-cli
        progress "Build completed in $((SECONDS - build_started))s."
    )
    target="${CARGO_TARGET_DIR:-$root/target}"
    [[ "$target" = /* ]] || target="$root/$target"
    binary="$target/release/rsi"
else
    progress "Using existing binary: $binary"
fi
[[ -f "$binary" && ! -L "$binary" ]] || { echo 'Binary must be an ordinary executable file.' >&2; exit 2; }
mkdir -p -- "$prefix/bin"
bin=$(cd -- "$prefix/bin" && pwd -P)
destination="$bin/rsi"
if [[ -e "$destination" || -L "$destination" ]]; then
    echo 'rsi is already installed here. Use rsi core upgrade --local PACKAGE, or choose another prefix.' >&2
    exit 2
fi
progress "Installing rsi to $destination..."
staged=$(mktemp "$bin/.rsi-install.XXXXXXXX")
trap 'rm -f -- "$staged"' EXIT
cp -- "$binary" "$staged"
chmod 755 "$staged"
cmp -s -- "$binary" "$staged" || { echo 'Binary changed while copying.' >&2; exit 2; }
progress 'Verifying executable...'
"$staged" --version
# Exclusive publication prevents two concurrent installers replacing each other.
ln -- "$staged" "$destination"
if $path_update; then
    progress 'Updating user PATH...'
    quoted=${bin//\'/\'\\\'\'}
    line="export PATH='$quoted':\"\$PATH\""
    for rc in "$HOME/.profile" "$HOME/.bashrc"; do
        grep -Fqx -- "$line" "$rc" 2>/dev/null || printf '\n%s\n' "$line" >> "$rc"
    done
    if [[ -f "$HOME/.zshrc" ]]; then
        grep -Fqx -- "$line" "$HOME/.zshrc" || printf '\n%s\n' "$line" >> "$HOME/.zshrc"
    fi
else
    progress 'Skipping PATH update (--no-path).'
fi
printf 'Installed %s\nOpen a new terminal and run rsi. Install optional packages with rsi plugin install.\n' "$destination"
