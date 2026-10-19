#!/usr/bin/env bash

set -Eeuo pipefail

readonly REPOSITORY_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
test_directory="$(mktemp -d -t gitwatch-version-test.XXXXXXXX)"
trap 'rm -rf -- "$test_directory"' EXIT
cp "$REPOSITORY_ROOT/version.sh" "$test_directory/version.sh"

assert_output() {
    local declared_version="$1" expected="$2"
    printf '%s\n' "$declared_version" >"$test_directory/VERSION"
    actual="$(GITWATCH_COMMIT_COUNT=42 bash "$test_directory/version.sh")"
    [[ "$actual" == "$expected" ]] || {
        printf 'expected %s to resolve to %s, got %s\n' \
            "$declared_version" "$expected" "$actual" >&2
        exit 1
    }
}

assert_output v0.2 v0.2
assert_output v1.2.1 v1.2.1
assert_output v0.3 v0.3.42

printf 'v0.3.1\n' >"$test_directory/VERSION"
if GITWATCH_COMMIT_COUNT=42 bash "$test_directory/version.sh" >/dev/null 2>&1; then
    printf 'odd-minor versions with an explicit patch must be rejected\n' >&2
    exit 1
fi

printf 'not-a-version\n' >"$test_directory/VERSION"
if bash "$test_directory/version.sh" >/dev/null 2>&1; then
    printf 'invalid versions must be rejected\n' >&2
    exit 1
fi

printf 'v0.2\n' >"$test_directory/VERSION"
bash "$test_directory/version.sh" --packageable
[[ "$(bash "$test_directory/version.sh" --package)" == "0.2" ]]

printf 'v0.3\n' >"$test_directory/VERSION"
if bash "$test_directory/version.sh" --packageable; then
    printf 'odd-minor versions must not be packageable\n' >&2
    exit 1
fi
