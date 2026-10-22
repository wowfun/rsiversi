#!/usr/bin/env bash
# Minimal installer. Application selection belongs to rsi.
set -euo pipefail
prefix="${HOME}/.local"
binary=''
path_update=true
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
        --prefix) (($# >= 2)) || { usage >&2; exit 2; }; prefix=$2; shift 2;;
        --binary) (($# >= 2)) || { usage >&2; exit 2; }; binary=$2; shift 2;;
        --no-path) path_update=false; shift;;
        *) usage >&2; exit 2;;
    esac
done
case "$(uname -s)" in
    Linux*) ;;
    *) echo 'Use PowerShell on Windows; source installation otherwise requires Linux/WSL.' >&2; exit 2;;
esac
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
if [[ -z "$binary" ]]; then
    (
        cd -- "$root"
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
        [[ ! -v CARGO_TARGET_DIR ]] || build_env+=("CARGO_TARGET_DIR=$CARGO_TARGET_DIR")
        [[ ! -v RUSTUP_TOOLCHAIN ]] || build_env+=("RUSTUP_TOOLCHAIN=$RUSTUP_TOOLCHAIN")
        env -i "${build_env[@]}" cargo build --locked --release -p rsi-cli
    )
    target="${CARGO_TARGET_DIR:-$root/target}"
    [[ "$target" = /* ]] || target="$root/$target"
    binary="$target/release/rsi"
fi
[[ -f "$binary" && ! -L "$binary" ]] || { echo 'Binary must be an ordinary executable file.' >&2; exit 2; }
mkdir -p -- "$prefix/bin"
bin=$(cd -- "$prefix/bin" && pwd -P)
destination="$bin/rsi"
if [[ -e "$destination" || -L "$destination" ]]; then
    echo 'rsi is already installed here. Use rsi core upgrade --local PACKAGE, or choose another prefix.' >&2
    exit 2
fi
staged=$(mktemp "$bin/.rsi-install.XXXXXXXX")
trap 'rm -f -- "$staged"' EXIT
cp -- "$binary" "$staged"
chmod 755 "$staged"
cmp -s -- "$binary" "$staged" || { echo 'Binary changed while copying.' >&2; exit 2; }
"$staged" --version
# Exclusive publication prevents two concurrent installers replacing each other.
ln -- "$staged" "$destination"
if $path_update; then
    quoted=${bin//\'/\'\\\'\'}
    line="export PATH='$quoted':\"\$PATH\""
    for rc in "$HOME/.profile" "$HOME/.bashrc"; do
        grep -Fqx -- "$line" "$rc" 2>/dev/null || printf '\n%s\n' "$line" >> "$rc"
    done
    if [[ -f "$HOME/.zshrc" ]]; then
        grep -Fqx -- "$line" "$HOME/.zshrc" || printf '\n%s\n' "$line" >> "$HOME/.zshrc"
    fi
fi
printf 'Installed %s\nOpen a new terminal and run rsi. Install optional packages with rsi plugin install.\n' "$destination"
