# gitwatch

`gitwatch` is a small, read-only terminal viewer for uncommitted Git changes. It
is designed to stay open beside a coding agent in a normal terminal, tmux, or
Byobu pane.

The core roadmap is complete through **Phase 4**, and the added Phase 6
documentation browser is also complete while Phase 5 remains pending.
Gitwatch provides navigable unified, before-file, and after-file views of the current
`HEAD`-to-working-tree changes, including staged, unstaged,
untracked, renamed, deleted, binary, large, symlink, and submodule states.
Native filesystem notifications update the view automatically after a quiet
period, with polling as a fallback for missed events and Git metadata changes.
Snapshot collection runs on a background thread so navigation remains
responsive during refreshes.

The current release is **v0.2**. Even minor versions are stable release versions
and are packaged by CI on pushes to `main`. Odd minor versions are development
versions: their displayed version automatically gains the Git commit count as a
patch component (for example, `v0.3.42`). An even-minor version may include an
explicit patch component, such as `v1.2.1`. The release version is declared in
[`VERSION`](VERSION); Cargo's package version uses the equivalent three-part
SemVer form required by Cargo.

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

On pushes to `main`, GitHub Actions uploads `.deb` and `.rpm` workflow artifacts
when the minor version in `VERSION` is even. Odd-minor development builds still
run the complete quality suite but skip package creation.

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
| `Tab` | Switch focus between file list and diff |
| `j` / `k`, arrows | Navigate the focused pane |
| `h` / `l`, left/right arrows | Scroll the diff horizontally |
| `/<text>` | Search the right pane (`Enter` to confirm, `Esc` to cancel) |
| `n` / `N` | Next / previous search match (after searching) |
| `Esc` | Clear an active search; otherwise quit |
| `Page Up` / `Page Down` | Move by a larger step |
| `Home` / `End` | Move to the beginning/end |
| `n` / `p` | Next / previous hunk when no search is active |
| `v` / `V` | Cycle forward / backward through unified, before, and after views |
| `t` | Open an interactive shell at the repository root; `exit` returns to Gitwatch |
| `d` | Toggle between live diffs and rendered repository documentation |
| `r` | Refresh immediately (normally unnecessary because refresh is automatic) |
| Left click | Focus a pane; clicking a file in the list also selects it |
| Mouse wheel | Scroll vertically when the pointer is over the diff pane |

When switching views with `v` or `V`, Gitwatch keeps a corresponding source line near
the tenth row of the viewport where possible. Changed lines retain diff-style
backgrounds: red for modified/deleted lines before and green for modified/added
lines after.

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
