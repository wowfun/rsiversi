#!/usr/bin/env bash
set -euo pipefail
usage() {
    printf '%s\n' 'Usage: bash uninstall.sh [--prefix DIR] [--uninstaller FILE] [--config-home DIR] [--state-home DIR] [--cache-home DIR] [--purge] [--dry-run] [--no-path] [--force] [--yes]' \
        'Remove an installation without running rsi or Cargo. Homes are preserved by default.' \
        '--purge clears shared runtime data and saved credentials, preserving user-authored configuration.' \
        '--force permits confirmed fixed-file removal without a valid record; it never force-kills a Host.' \
        'After uninstall, later --purge requires an external prebuilt --uninstaller FILE.'
}
for argument in "$@"; do case "$argument" in --help|-h) usage; exit 0;; esac; done
prefix=''; uninstaller=''; purge=false
arguments=(); windows=()
while (($#)); do
    case "$1" in
        --prefix|--uninstaller|--config-home|--state-home|--cache-home)
            (($# >= 2)) && [[ -n "$2" ]] || { usage >&2; exit 2; }
            case "$1" in --prefix) prefix=$2; flag=-Prefix;; --uninstaller) uninstaller=$2; flag=-Uninstaller;; --config-home) flag=-ConfigHome;; --state-home) flag=-StateHome;; --cache-home) flag=-CacheHome;; esac
            windows+=("$flag" "$2"); if [[ "$1" != --uninstaller ]]; then arguments+=("$1" "$2"); fi; shift 2;;
        --purge|--dry-run|--no-path|--force|--yes)
            case "$1" in --purge) purge=true; flag=-Purge;; --dry-run) flag=-DryRun;; --no-path) flag=-NoPath;; --force) flag=-Force;; --yes) flag=-Yes;; esac
            windows+=("$flag"); arguments+=("$1"); shift;;
        *) usage >&2; exit 2;;
    esac
done
case "$(uname -s)" in
    MINGW*|MSYS*)
        for tool in cygpath powershell.exe; do command -v "$tool" >/dev/null || { echo "Git Bash uninstall requires $tool." >&2; exit 2; }; done
        converted=()
        for ((index=0; index<${#windows[@]}; index++)); do
            value=${windows[index]}; converted+=("$value")
            case "$value" in -Prefix|-Uninstaller|-ConfigHome|-StateHome|-CacheHome) ((index+=1)); converted+=("$(cygpath -aw -- "${windows[index]}")");; esac
        done
        script=$(cygpath -aw -- "$(dirname -- "${BASH_SOURCE[0]}")/uninstall.ps1")
        MSYS2_ARG_CONV_EXCL='*' exec powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$script" "${converted[@]}";;
    Linux*) ;;
    *) echo 'Uninstall supports Linux/WSL and Windows Git Bash/PowerShell.' >&2; exit 2;;
esac
if [[ -z "$prefix" ]]; then
    directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
    if [[ "$directory" == */libexec/rsi ]]; then prefix=${directory%/libexec/rsi}; else prefix="$HOME/.local"; fi
    arguments+=(--prefix "$prefix")
fi
[[ "$prefix" = /* ]] || prefix="$PWD/$prefix"
if [[ -z "$uninstaller" ]]; then uninstaller="$prefix/libexec/rsi/rsi-uninstall"; fi
if [[ ! -e "$uninstaller" && ! -L "$uninstaller" ]]; then
    remaining=false
    for name in bin/rsi libexec/rsi/rsi-uninstall libexec/rsi/uninstall.sh libexec/rsi/install.json bin/.rsi-core-upgrade.pending.json; do
        [[ ! -e "$prefix/$name" && ! -L "$prefix/$name" ]] || remaining=true
    done
    if ! $purge && ! $remaining; then printf 'No installation files found at %s; homes were not cleaned.\n' "$prefix"; exit 0; fi
    echo 'Uninstaller missing. Supply --uninstaller FILE from a trusted prebuilt core package; no working rsi or Cargo is required.' >&2; exit 2
fi
[[ -f "$uninstaller" && ! -L "$uninstaller" ]] || { echo 'Uninstaller must be an ordinary file.' >&2; exit 2; }
for base in "$prefix" "$(dirname -- "$uninstaller")"; do
    [[ "$base" = /* ]] || base="$PWD/$base"
    while [[ "$base" != / ]]; do
        if [[ -L "$base" ]]; then printf 'Path traverses a link at %s; pass its resolved directory explicitly.\n' "$base" >&2; exit 2; fi
        if [[ -e "$base" && ! -d "$base" ]]; then printf 'Parent is not a directory: %s\n' "$base" >&2; exit 2; fi
        base=$(dirname -- "$base")
    done
done
exec "$uninstaller" uninstall --interface-version 1 "${arguments[@]}"
