# gitwatch

`gitwatch` is a small, read-only terminal viewer for uncommitted Git changes. It
is designed to stay open beside a coding agent in a normal terminal, tmux, or
Byobu pane.

The project is currently at **Phase 2**. It provides a navigable unified diff of
the current `HEAD`-to-working-tree changes, including staged, unstaged,
untracked, renamed, deleted, binary, large, symlink, and submodule states.
Refreshes are manual until the live watcher arrives in Phase 3.

## Build and run

```console
cargo build
cargo run -- --help
cargo run -- /path/to/repository
```

For a release build, run the quickstart script and select exactly one output:
the executable alone, a Debian package, or an RPM package.

```console
./quickstart.sh
```

The same choices can be supplied non-interactively. Package artifacts are placed
in `target/packages/`.

```console
./quickstart.sh exe
./quickstart.sh deb
./quickstart.sh rpm
```

Debian packaging requires `dpkg-deb`. RPM packaging requires `rpmbuild` (usually
provided by `rpm-build` or `rpmdevtools`). The script always builds the release
executable first and produces only the selected package format.

The optional refresh timings default to the policy in the project blueprint:

```text
--quiet-ms 2000
--max-delay-ms 10000
--poll-ms 2000
```

## Keyboard controls

| Key | Action |
|---|---|
| `q` | Quit |
| `Esc` | Quit |
| `Tab` | Switch focus between file list and diff |
| `j` / `k`, arrows | Navigate the focused pane |
| `h` / `l`, left/right arrows | Scroll the diff horizontally |
| `Page Up` / `Page Down` | Move by a larger step |
| `Home` / `End` | Move to the beginning/end |
| `n` / `p` | Next / previous hunk |
| `r` | Refresh immediately |

Side-by-side and top-bottom modes (`2` and `3`) are planned for Phase 4.

## Safety

`gitwatch` is observational. It does not stage, commit, reset, check out, or edit
repository files. Git subprocesses are constrained to an explicit query-command
allowlist and run with prompting, pagers, and optional index locks disabled.

## Development

```console
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

See [`docs/BLUEPRINT.md`](docs/BLUEPRINT.md) and
[`docs/plan/DEVELOPMENT_PLAN.md`](docs/plan/DEVELOPMENT_PLAN.md) for the complete
product definition and phased implementation plan.

## License

MIT
