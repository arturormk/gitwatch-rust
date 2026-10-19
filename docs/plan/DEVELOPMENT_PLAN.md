# Development Plan — gitwatch

## 1. Goal and product boundary

Build `gitwatch`, a small Linux terminal application that gives a developer
situational awareness of uncommitted changes while another process edits a Git
working tree. It shows the current delta from `HEAD`, updates without blocking
keyboard input, and returns to an empty state after those changes are committed.

The first release is successful when it is dependable enough to leave running
in a tmux or Byobu pane. It is an observer, not a review or repository-management
tool. It will never edit files, update the index, stage, commit, reset, switch
branches, or retain a pre-commit baseline.

The executable is named `gitwatch`; the repository/project name is
`gitwatch-rust`.

## 2. Version-one decisions

These choices keep the initial implementation focused while satisfying the
blueprint.

- Use a flat, sorted changed-file list. Display repository-relative paths, so
  directory structure remains obvious without implementing expandable tree
  state. A tree can be reconsidered only after the core viewer is stable.
- Use the system `git` executable for repository discovery and status. Use
  NUL-delimited machine-readable output and never parse human-oriented output.
- Build one normalized diff model, then render unified, side-by-side, and
  top-bottom views from it. Presentation modes do not launch their own Git
  commands.
- Use `ratatui` and `crossterm` for the interface, `notify` for filesystem hints,
  `similar` for line alignment, and `syntect` for syntax highlighting.
- Use a standard thread/channel architecture rather than an async runtime. Git
  and filesystem work runs outside the UI thread; the UI thread owns all view
  state and terminal rendering.
- Compute complete snapshots on refresh for correctness. Add explicit size
  limits and avoid work when the repository fingerprint is unchanged. More
  elaborate incremental caching is deferred until measurements justify it.
- Target stable Rust on Linux. macOS or Windows support may work through the
  chosen crates, but is not a version-one acceptance requirement.

## 3. Proposed crate and module layout

```text
Cargo.toml
README.md
src/
  main.rs          CLI parsing, startup, terminal restoration, exit status
  cli.rs           path and timing arguments
  app.rs           UI-owned state and state transitions
  model.rs         repository snapshot, file changes, hunks, rows, anchors
  git.rs           repository discovery and read-only Git subprocesses
  snapshot.rs      status-to-file-content/diff snapshot assembly
  diff.rs          normalized line diff and hunk construction
  highlight.rs     syntax selection and terminal-safe style composition
  watcher.rs       notify integration and event filtering
  scheduler.rs     quiet-period/max-delay/poll refresh state machine
  worker.rs        refresh requests, generations, results, cancellation policy
  input.rs         crossterm event mapping
  ui.rs            layout and widgets
tests/
  support/mod.rs   temporary repository fixture and Git helpers
  git_snapshot.rs
  live_refresh.rs
```

Modules may be combined if they remain small. In particular, `snapshot.rs` can
start inside `git.rs`, and `worker.rs` can start inside `app.rs`; they should be
split only when the implementation becomes clearer as a result.

## 4. Core data model

Keep repository facts separate from view state.

```text
RepositorySnapshot
  generation
  repository_root
  head_identity             commit ID or empty-tree marker
  collected_at
  files: Vec<FileChange>
  warnings: Vec<SnapshotWarning>

FileChange
  key                       stable identity used across refreshes
  old_path: Option<PathBuf>
  new_path: Option<PathBuf>
  status: Modified | Added | Deleted | Renamed | TypeChanged | Untracked
  staged: bool
  unstaged: bool
  kind: Text | Binary | TooLarge | Submodule | Symlink | Unavailable
  old_content/new_content   bounded byte/text representation
  diff: FileDiff

FileDiff
  hunks: Vec<DiffHunk>

DiffHunk
  old/new starting line and length
  rows: Vec<DiffRow>

DiffRow
  Equal | Delete | Insert | Replace
  optional old and new line number/content
```

`DiffRow` is the common source for all renderers. Unified mode emits context,
deletion, and insertion rows; side-by-side places the optional old/new cells on
one row; top-bottom groups the old and new sides of each hunk. This makes hunk
navigation identical in every mode.

`AppState` contains focus, selected file key/index, mode, per-file/per-mode scroll
positions, terminal dimensions, pending/refresh/error indicators, and the latest
accepted snapshot generation. Repository data remains immutable after a worker
publishes it.

For refresh preservation, use a file key based on the current path, with the old
path as a rename fallback. Preserve a logical anchor where possible: selected
hunk plus nearby old/new source line. Fall back to proportional vertical offset,
then clamp. Preserve horizontal offset independently for each presentation mode.

## 5. Read-only Git and snapshot strategy

### Repository discovery

1. Canonicalize/validate the requested path without requiring it to be the root.
2. Run `git rev-parse --show-toplevel` from that path.
3. Detect and report bare repositories with a concise startup error.
4. Obtain the Git directory/common directory so watcher filters work with normal
   repositories and linked worktrees.
5. Resolve `HEAD` with `git rev-parse --verify HEAD`. Treat a missing `HEAD` as an
   unborn repository rather than an error.

Every Git subprocess will:

- use argument arrays, not a shell;
- set `GIT_TERMINAL_PROMPT=0` and `GIT_OPTIONAL_LOCKS=0`;
- pass `--no-optional-locks` where supported;
- clear pager behavior with `GIT_PAGER=cat`/`PAGER=cat`;
- avoid external diff drivers with `--no-ext-diff` and avoid text-conversion
  filters where relevant;
- capture stdout/stderr and enforce bounded output/time handling;
- never invoke a command documented to mutate the worktree, refs, configuration,
  or index.

Add a test that hashes or records index metadata before and after repeated
snapshots to defend the read-only guarantee.

### Change discovery

Use `git status --porcelain=v2 -z --untracked-files=all` as the authoritative
changed-path inventory. Parse ordinary, rename/copy, unmerged, and untracked
records without converting paths through lossy UTF-8. Keep paths as `OsString` or
raw platform bytes until display, where invalid bytes can be escaped.

The porcelain XY state records whether a path has staged changes, unstaged
changes, or both. Rename records retain both paths. Ignored files are absent by
construction. Define deterministic sorting on escaped repository-relative path.

### Content and diff construction

For each discovered path, obtain the baseline and observed content:

- tracked/renamed/deleted: baseline content and mode come from the current
  `HEAD` tree;
- untracked or an unborn repository: baseline is empty;
- non-deleted files: observed content comes from the working-tree path;
- deleted files: observed content is empty.

Use non-mutating Git object reads for baseline blobs, keyed by tree/object IDs
reported by Git rather than interpolating arbitrary paths into revision syntax.
Read working-tree files directly with bounded I/O and verify they still refer to
the expected file type. A file changing during collection is not fatal: mark the
snapshot potentially stale and schedule another refresh.

Run binary detection before UTF-8/text diffing. Clearly label binary files,
submodules, symlinks, mode-only changes, unavailable files, and files beyond the
configured internal safety limit. Lossy display is acceptable for text that is
not valid UTF-8, but path identity must never be lossy.

Use `similar` to align old and new lines and form hunks with a small fixed context
(initially three lines). Preserve final-newline information. Rename-only changes
can show a metadata message; renamed-and-edited files also show their content
diff. This approach deliberately represents `HEAD` versus the current working
tree, so a file with both staged and unstaged edits appears once with both status
flags.

Before implementation, validate the exact porcelain-v2 fields and object lookup
commands against Git's installed documentation and fixture repositories. Put the
parser behind fixture tests so command details can change without affecting the
UI or diff model.

## 6. Live-update and concurrency design

The UI thread runs a single event loop and never waits for Git or file reads.
Inputs arrive through channels:

```text
crossterm input/resize -----> UI event loop <----- worker result
                                  ^
                                  |
notify hints ---> refresh scheduler <--- 2 s reconciliation tick
                                  |
                           refresh request
                                  |
                         single snapshot worker
```

The scheduler is a small state machine with injectable time:

- `Idle`: no pending filesystem-triggered refresh.
- `Pending { first_event, last_event }`: refresh at the earlier of
  `last_event + quiet_period` and `first_event + max_delay`.
- `Refreshing`: one worker request is active; new events set a dirty flag.

Filesystem hints start/reset the quiet timer. The two-second poll is a
reconciliation trigger: collect or compare a cheap repository fingerprint, but
do not move an already scheduled filesystem deadline. Manual `r` bypasses the
debounce. During a refresh, coalesce further requests into exactly one follow-up
refresh.

Assign monotonically increasing generations when requests are made. The UI only
accepts a result if it is not older than the most recently accepted/requested
state according to the worker protocol. Because there is one worker, results are
ordered; generations still make stale-result behavior explicit and testable. If
events arrive while generation N is running, accept N only as an interim snapshot
when useful and immediately request N+1, or discard N if a newer request has
already been issued.

Watch the working tree recursively. Also watch the minimal Git metadata needed to
notice commits/index/ref changes in normal repositories and worktrees. Filter
bulk `.git` noise, temporary editor files, and events outside the repository;
correctness comes from polling when event filtering misses a platform-specific
case. A commit must produce or soon be followed by a reconciliation that changes
the `HEAD` identity and yields a clean snapshot.

Redraw only for input that changes view state, resize, pending/status indicator
changes, or an accepted refresh result. Restore the terminal on normal exit,
errors, and panic as far as safely possible.

## 7. Interface behavior

The initial layout has a one-line header, changed-file list on the left, diff on
the right, and a one-line help footer. The left pane should use roughly 25–30% of
the width with reasonable minimums; the diff receives the remainder.

- Header: `gitwatch`, compact repository path/name, mode, and one of
  `live`, `pending`, `refreshing`, or `error`.
- File list: concise status (`M`, `A`, `D`, `R`, `?`) plus staged/unstaged
  distinction without making the pane noisy. Show the full path in a title or
  status area when truncated.
- Diff: syntax-aware rows, source line numbers, hunk headers, visible additions
  and deletions, and an explicit message for non-text/limited content.
- Footer: `Tab focus  1/2/3 mode  n/p hunk  r refresh  q quit`, adapted when
  width is limited.
- Empty state: `Working tree clean — waiting for changes...`.
- Error state: retain the last good snapshot, show the error non-modally, and
  keep retrying. Startup errors before a valid repository exists may exit.

Do not silently replace side-by-side mode in a narrow terminal. Instead, retain
the mode, give each side a minimal viewport, allow horizontal scrolling, and show
a short narrow-terminal hint if content is severely constrained.

Input mapping follows the blueprint. Arrow keys and `j`/`k` act on the focused
pane; Page Up/Down, Home/End are added as conventional aliases. File selection
immediately swaps the diff without refreshing repository data. `n`/`p` move to
hunk anchors in the selected file. A left click selects a visible file, and the
mouse wheel scrolls vertically only while the pointer is over the diff pane.

The `t` key temporarily restores the normal terminal, launches `$SHELL` at the
repository root (falling back to `/bin/sh`), and resumes the same TUI state when
the shell exits. Mouse capture must be disabled while the shell is active and
restored with raw mode and the alternate screen afterward.

Syntax highlighting is chosen from the displayed path/extension, with a plain
text fallback. Compose syntax foreground styles with diff background/emphasis so
add/delete meaning remains dominant and readable on dark and light terminals.

## 8. Delivery phases

Each phase ends in a usable, tested increment. Do not postpone tests to the final
phase.

### Phase 0 — Scaffold and safeguards

**Status: Complete.**

- Create the binary crate, minimal CLI, module skeleton, license choice, and
  concise README outline.
- Add formatting and lint settings without unusually strict policy.
- Add a terminal-session guard that always leaves alternate screen/raw mode.
- Implement a subprocess wrapper with the noninteractive/read-only environment.
- Establish CI for stable Rust: format check, Clippy with warnings denied for
  project code, and tests.

Exit criterion: `gitwatch --help` and `--version` work; CI/local quality commands
pass; an empty TUI can enter and restore the terminal safely.

### Phase 1 — Repository snapshot engine

**Status: Complete.**

- Discover roots from the current directory or a supplied subdirectory.
- Parse porcelain-v2 `-z` status, including rename records and unusual paths.
- Resolve `HEAD`/unborn state and construct old/new file content safely.
- Detect binary, large, deleted, untracked, symlink, submodule, and mode-only
  cases.
- Produce normalized hunks and aligned rows.

Exit criterion: integration tests in temporary repositories prove discovery and
`HEAD`-to-working-tree behavior for clean, staged, unstaged, combined, untracked,
deleted, renamed, unborn, and unusual-filename cases. Repeated collection leaves
the index/repository unchanged.

### Phase 2 — Navigable unified viewer

**Status: Complete.**

- Render header, flat file list, unified diff, footer, empty/error states.
- Implement focus, file selection, vertical/horizontal scrolling, hunk
  navigation, resize handling, and `q`/`r`.
- Support mouse file selection, diff scrolling, and a temporary interactive
  shell opened with `t`.
- Preserve file selection and clamp/restore scroll anchors when a new snapshot is
  applied.
- Add syntax highlighting after basic unified rendering is correct.

Exit criterion: a user can launch from a repository subdirectory, inspect every
current change by keyboard or mouse, run a quick command in the temporary shell,
resize the terminal, and quit with the terminal restored.

### Phase 3 — Live refresh

**Status: Complete.**

- Add filesystem watcher and relevant Git metadata watches.
- Implement scheduler state machine, quiet period, maximum delay, polling,
  manual bypass, generation handling, coalescing, and background worker.
- Surface pending/refresh/error status without blocking input.
- Ensure a commit clears the display on the next event or polling cycle.

Exit criterion: deterministic scheduler tests prove trailing-edge debounce and
the ten-second cap. Integration/manual tests show rapid edits, atomic saves, and
commits update correctly while navigation remains responsive.

### Phase 4 — Alternate presentations

**Status: Not started.**

- Render side-by-side aligned old/new cells from `DiffRow`.
- Render top-bottom before/after hunk sections from the same rows.
- Preserve mode while changing files and refreshing; maintain per-mode scrolling.
- Verify long-line and narrow-terminal behavior without silently changing mode.

Exit criterion: keys `1`, `2`, and `3` switch correct views immediately, and
hunk navigation refers to the same logical hunks in all modes.

### Phase 5 — Hardening and release readiness

**Status: Not started.**

- Exercise worktrees, temporary repository disappearance, concurrent writes,
  large/binary files, symlinks, submodules, mode changes, and Git failures.
- Profile a representative repository; remove avoidable idle redraw/diff work.
- Complete README installation, usage, shortcuts, behavior, and known limits.
- Run manual smoke tests in a normal terminal and tmux/Byobu.
- Run formatting, Clippy, all tests, and a release build.

Exit criterion: every blueprint acceptance criterion is either demonstrated by
an automated test or recorded manual check, with no repository mutation observed.

## 9. Test plan

### Unit tests

- Porcelain-v2 NUL parser: every relevant record, truncated/invalid records,
  spaces, tabs, newlines, Unicode, and non-UTF-8 paths on Unix.
- Diff model: context grouping, replace alignment, insert/delete-only hunks,
  trailing newline, side-by-side blank cells, and top-bottom grouping.
- Scheduler: isolated event, burst, continuous events reaching maximum delay,
  poll interaction, event during refresh, manual refresh, and result generations.
- App reducer: selection survival, nearest-file fallback, clean transition,
  per-mode offsets, hunk anchors, and resize clamping.
- Rendering calculations: narrow widths, Unicode display width, line-number
  gutters, long-line horizontal offsets, and status truncation.

Use a fake/injectable clock for scheduler tests and pure state-transition
functions for app behavior.

### Integration tests

Create isolated temporary repositories with local test-only author configuration.
Test:

1. clean and unborn repositories;
2. staged, unstaged, and combined changes;
3. untracked, deleted, renamed, binary, and ignored files;
4. paths containing spaces, tabs, newlines, Unicode, and Unix non-UTF-8 bytes;
5. a commit removing displayed changes, followed by new edits;
6. discovery from a nested directory and a linked worktree;
7. repository changes during snapshot collection;
8. index checksum/metadata stability across repeated collection;
9. worker failure and recovery;
10. watcher-driven atomic replacement and polling fallback.

Timing tests should use scheduler injection wherever possible. A small number of
real watcher tests may use bounded waits and retry loops, never long fixed sleeps.

### Manual acceptance matrix

- Terminal: a conventional Linux terminal, tmux, and Byobu.
- Width: normal, narrow, and resize while each diff mode is active.
- Activity: idle, burst saves, continuous writes, commit, and edits after commit.
- Navigation: file changes while selected, selected file disappears, all changes
  disappear, hunk navigation, and long-line scrolling.
- Safety: compare `git status --porcelain=v2`, `HEAD`, refs, config, and index
  metadata before and after an extended run.

## 10. Performance and safety limits

Choose initial limits during implementation and document them in code and the
README. Reasonable starting policy:

- bound captured Git stdout/stderr;
- cap individual text files/diffs before full in-memory rendering;
- cap rendered line width while retaining horizontal navigation within the
  supported bound;
- show an informative placeholder instead of freezing or allocating without
  limit;
- retain the previous good snapshot on transient collection errors;
- do no work between poll deadlines when there are no UI or watcher events.

Limits are internal safeguards, not configuration flags in the first release.
Measure before choosing exact byte/line values so normal source files are not
surprisingly hidden.

## 11. Risks and mitigations

| Risk | Mitigation |
|---|---|
| A nominally read-only Git command refreshes the index | Set optional locks off, test index stability, and keep all commands behind one audited wrapper. |
| Files change midway through a snapshot | Treat snapshots as eventually consistent, detect obvious metadata changes, and immediately schedule a follow-up. |
| Watcher misses commit/ref events or emits a storm | Watch minimal metadata, filter hints, coalesce requests, and reconcile by polling. |
| Stale background results disrupt reading | Use request generations, a single worker, immutable snapshots, and explicit anchor restoration. |
| Side-by-side alignment diverges from unified output | Generate all modes from the same normalized `DiffRow`/hunk model and cross-test them. |
| Syntax and diff styles become unreadable | Make diff semantics dominant, use conservative colors, and manually test light/dark terminals. |
| Unusual paths corrupt parsing or selection | Parse `-z` bytes, retain lossless path identity, and escape only for display. |
| Large/binary content freezes the TUI | Detect early, apply bounded reads/output, and render explanatory placeholders. |
| Terminal remains in raw/alternate-screen mode after failure | Central terminal guard plus panic/error restoration and manual interruption tests. |

## 12. Deferred items

The following are intentionally outside the first release unless core testing
shows they are required:

- collapsible directory tree;
- user configuration/theme files;
- persistent state or historical/session baselines;
- staging, editing, commit, branch, or remote operations;
- plugin architecture or library API;
- multiple repositories in one process;
- platform-specific optimizations or an embedded Git implementation.

## 13. Final definition of done

Before declaring the first release complete:

- all three modes correctly represent `HEAD` versus the current working tree;
- staged, unstaged, untracked, deleted, and renamed files are discoverable;
- watcher debounce, maximum delay, poll reconciliation, and manual refresh behave
  as specified;
- input and resize remain responsive during refreshes;
- selection, mode, and useful reading position survive refreshes;
- a commit returns the UI to the clean state and later edits start a new slate;
- binary/large/error states are informative rather than disruptive;
- tests demonstrate unusual-path handling and the read-only guarantee;
- `cargo fmt --check`, Clippy, tests, and `cargo build --release` pass on stable
  Rust/Linux;
- README instructions and shortcuts match the shipped behavior;
- a final manual run in terminal plus tmux/Byobu satisfies the blueprint's
  acceptance criteria without changing repository or index state.
