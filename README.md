# gitwatch

`gitwatch` is a small, read-only terminal viewer for uncommitted Git changes. It
is designed to stay open beside a coding agent in a normal terminal, tmux, or
Byobu pane.

The core roadmap is complete through **Phase 3**, and the added Phase 6
documentation browser is also complete while Phases 4 and 5 remain pending.
Gitwatch provides a navigable unified diff of the current
`HEAD`-to-working-tree changes, including staged, unstaged,
untracked, renamed, deleted, binary, large, symlink, and submodule states.
Native filesystem notifications update the view automatically after a quiet
period, with polling as a fallback for missed events and Git metadata changes.
Snapshot collection runs on a background thread so navigation remains
responsive during refreshes.

Press `d` to open the repository documentation browser. It lists every regular
`*.md` file below the repository root—including hidden and ignored files—and
renders the selected document in the right pane. Git metadata directories and
directory symlinks are not traversed. The open document is reloaded after
on-disk edits using the same live-refresh cycle as the diff view; press `d`
again to return without losing either view's selection or reading position.

## Build and run

```console
cargo build
cargo run -- --help
cargo run -- /path/to/repository
```

For a release build, run the quickstart script and select exactly one output:
the executable alone, a Debian package, or an RPM package.
Pressing Enter at the prompt selects the release executable (option 1).

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

Filesystem changes use trailing-edge debouncing: Gitwatch refreshes after the
quiet period, but continuous activity cannot postpone a refresh beyond the
maximum delay. Pressing `r` requests an immediate refresh; if a refresh is
already running, it queues one follow-up refresh.

## Keyboard and mouse controls

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
| `t` | Open an interactive shell at the repository root; `exit` returns to Gitwatch |
| `d` | Toggle between live diffs and rendered repository documentation |
| `r` | Refresh immediately |
| Left click | Select a file from the file list |
| Mouse wheel | Scroll vertically when the pointer is over the diff pane |

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
