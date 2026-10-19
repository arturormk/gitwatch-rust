#!/usr/bin/env bash

set -Eeuo pipefail

readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"

die() {
    printf 'version: %s\n' "$*" >&2
    exit 1
}

version="$(tr -d '[:space:]' <"$SCRIPT_DIR/VERSION")"
if [[ ! "$version" =~ ^v([0-9]+)\.([0-9]+)(\.([0-9]+))?$ ]]; then
    die "VERSION must have the form vMAJOR.MINOR or vMAJOR.MINOR.PATCH"
fi

minor="${BASH_REMATCH[2]}"
patch="${BASH_REMATCH[4]:-}"
minor_number="$((10#$minor))"

case "${1:-}" in
    "" | --package | --packageable) ;;
    *) die "usage: ./version.sh [--package|--packageable]" ;;
esac

if [[ "${1:-}" == "--packageable" ]]; then
    [[ -z "$patch" || $((minor_number % 2)) -eq 0 ]] || \
        die "odd-minor development versions must not specify a patch"
    ((minor_number % 2 == 0))
    exit
fi

if ((minor_number % 2 == 1)); then
    [[ -z "$patch" ]] || die "odd-minor development versions must not specify a patch"
    commit_count="${GITWATCH_COMMIT_COUNT:-}"
    if [[ -z "$commit_count" ]]; then
        command -v git >/dev/null 2>&1 || die "git is required for odd-minor versions"
        commit_count="$(git -C "$SCRIPT_DIR" rev-list --count HEAD)"
    fi
    [[ "$commit_count" =~ ^[0-9]+$ ]] || die "commit count must be a non-negative integer"
    version="${version}.${commit_count}"
fi

case "${1:-}" in
    "") printf '%s\n' "$version" ;;
    --package) printf '%s\n' "${version#v}" ;;
esac
