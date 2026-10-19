# BLUEPRINT.md — gitwatch

## 1. Project Overview

**Project:** `gitwatch`
**Language:** Rust
**Developer:** Codex
**Application type:** Terminal User Interface (TUI)
**Primary purpose:** Live, read-only visualization of changes made to a Git working tree.

`gitwatch` is a lightweight terminal application intended to run alongside an AI coding agent such as Codex or Claude Code.

The user typically runs the coding agent in one terminal window and `gitwatch` in another. In terminal-only environments, both may run in separate Byobu or tmux windows or panes.

As the agent modifies files, `gitwatch` automatically displays the changed files and their syntax-highlighted diffs.

The application is strictly observational. It does not stage, commit, revert, edit, or otherwise modify repository contents.

**Design philosophy:**

- One purpose, executed well.
- Minimal configuration.
- Fast startup and low resource usage.
- No AI or LLM integration.
- No network services or telemetry.
- No unnecessary features.
- Prefer established Rust libraries over custom implementations.
- Do not introduce complexity to support hypothetical future requirements.

The user values the simplicity of terminal-based coding agents and does not want to return to a GUI development environment.

---

## 2. Core Requirements

### 2.1 Changed-file browser

Display a navigable list of files that have changed relative to the repository's current `HEAD`.

Include:

- Modified tracked files.
- Added files, including untracked files.
- Deleted tracked files.
- Renamed files, where Git detects renames.
- Files with staged changes, unstaged changes, or both.

A directory tree is preferred, but a simple list with relative paths is acceptable if it significantly simplifies implementation.

If a tree is implemented:

- Directories should be collapsible.
- Only directories containing changed files should appear.
- Files should have concise status indicators (`M`, `A`, `D`, `R`, `?`).
- Selection should survive refreshes when the selected file still exists.

When a file is selected, display its diff in the adjacent pane.

Selecting files must never change repository state.

### 2.2 Diff display

Provide three presentation modes:

1. **Unified:** Conventional Git-style diff with context, additions, and deletions.
2. **Side-by-side:** Original content on the left and modified content on the right, with corresponding lines aligned.
3. **Top-bottom:** Original and modified content displayed vertically, with clear correspondence between changed sections.

Switch modes using:

- `1` — Unified.
- `2` — Side-by-side.
- `3` — Top-bottom.

The selected mode should remain active when switching files.

All modes should provide:

- Syntax highlighting appropriate to the file type.
- Clearly distinguishable additions and deletions.
- Line numbers, where applicable.
- Context surrounding changes.
- Vertical navigation.
- Horizontal scrolling or another sensible treatment of long lines.
- Navigation between changed hunks.

Use an established syntax-highlighting library such as `syntect`.

Use Git's diff output or an established diff library as the foundation. Avoid implementing a new diff algorithm.

Side-by-side mode should align corresponding lines using diff information, inserting visual blank rows where necessary.

Top-bottom mode may present corresponding before/after hunks in vertically stacked sections rather than duplicating entire files.

The application must handle narrow terminals gracefully. Do not silently change the user's requested presentation mode.

### 2.3 Live updates

Automatically detect modifications to the working tree and refresh the displayed information.

**Default update policy:**

| Parameter | Default |
|---|---|
| Filesystem quiet period | 2 seconds |
| Maximum delay during continuous changes | 10 seconds |
| Git status polling interval | 2 seconds |
| Manual refresh | Immediate |

Implement trailing-edge debouncing:

1. Detect a filesystem change.
2. Start a 2-second quiet-period timer.
3. Reset the timer when another relevant change occurs.
4. Refresh after 2 seconds without further changes.
5. If changes continue, refresh no later than 10 seconds after the first pending change.

Prefer filesystem notifications using the `notify` crate.

Use periodic polling as a fallback and to detect Git state changes that filesystem notifications may miss.

A polling event should not unnecessarily restart the quiet-period timer.

**Important:** The TUI must remain responsive to user input while waiting for refreshes or executing Git operations.

Use asynchronous workers, channels, or an equivalent nonblocking architecture as appropriate.

Avoid continuously regenerating diffs or redrawing the entire interface while nothing has changed.

### 2.4 Git behavior

The viewer displays changes relative to the current `HEAD`.

This includes both staged and unstaged modifications.

The effective comparison is:

- Tracked files: `HEAD` versus current working-tree content.
- Untracked files: empty file versus current content.
- Deleted files: `HEAD` content versus empty file.

Do not rely exclusively on `git diff`, since it excludes untracked files and does not include staged changes by default.

Use Git status information to discover all changed paths, including staged changes.

**Commit semantics are intentionally simple:**

When the coding agent commits its changes, those changes disappear from the viewer because they are no longer differences relative to the current `HEAD`.

There is no historical review mode, persistent baseline, or session tracking.

If the agent commits and subsequently makes more changes, display only the new uncommitted changes.

Handle Git repositories without an initial commit gracefully. An empty-tree baseline is an acceptable implementation.

Do not automatically fetch, pull, push, checkout, commit, stage, stash, or reset anything.

Git operations must be observational.

For Git subprocesses, disable interactive prompting and use noninteractive execution. Do not execute user-defined external diff tools or Git hooks.

### 2.5 Preserving the user's reading position

Live updates should not unnecessarily disrupt navigation.

When refreshing:

- Keep the currently selected file if it still exists.
- Preserve the current diff presentation mode.
- Preserve the approximate vertical and horizontal scroll positions.
- Clamp scroll positions when the diff becomes shorter.
- If the selected file disappears, select the nearest remaining file.
- If all changes disappear, show a clean working-tree message.

Prefer preserving a visible diff hunk or source-line anchor when practical, but do not introduce excessive complexity for the first version.

### 2.6 Keyboard and mouse controls

| Key | Action |
|---|---|
| `↑` / `↓` | Navigate within the focused pane |
| `Tab` | Switch focus between file list and diff |
| `t` | Open an interactive shell at the repository root; `exit` returns to the TUI |
| `j` / `k` | Scroll or navigate within focused pane |
| `1` | Unified diff |
| `2` | Side-by-side diff |
| `3` | Top-bottom diff |
| `n` / `p` | Next / previous diff hunk |
| `←` / `→` | Horizontal scrolling when appropriate |
| `r` | Force immediate refresh |
| `q` | Quit |
| Left click | Select a file from the file list |
| Mouse wheel | Scroll vertically when the pointer is over the diff pane |

Additional conventional TUI navigation keys, such as Page Up, Page Down, Home, and End, are welcome.

Avoid excessive shortcuts or modal interfaces.

Display a compact keyboard-help line along the bottom of the screen.

---

## 3. User Interface

The interface should have two primary regions.

### Left pane: Changed files

A relatively narrow pane containing the changed-file list or tree.

Show status indicators and filenames.

Directories should not consume more space than necessary.

### Right pane: Diff

The larger pane displaying the selected file's diff.

The default presentation mode should be unified, with the user able to switch modes immediately.

The right pane should maximize the available width, especially for side-by-side comparisons.

### Header

Display:

- Application name.
- Repository name or path.
- Current diff mode.
- Optional small live/pending/error indicator.

### Footer

Display concise keyboard shortcuts and, optionally, the number of changed files.

### Empty state

When no changes are present, show something similar to:

`Working tree clean — waiting for changes...`

There is no need for animations, spinners, or continuous redraws.

### Resize behavior

Support terminal resize events without restarting the application.

Recalculate layouts and clamp scroll positions appropriately.

Ensure compatibility with conventional terminals and tmux/Byobu.

Avoid requiring terminal-specific features beyond standard modern ANSI terminal capabilities.

---

## 4. Suggested Rust Architecture

Prefer a small modular architecture rather than a large framework.

Suggested crates:

| Purpose | Crate / tool |
|---|---|
| Terminal rendering | `ratatui` |
| Terminal input | `crossterm` |
| Filesystem notifications | `notify` |
| Syntax highlighting | `syntect` |
| Diff representation/alignment | `similar`, if appropriate |
| Git status and diffs | Git CLI initially |
| Error handling | `anyhow` or `thiserror` |
| Command-line arguments | `clap`, if useful |

These are suggestions, not mandatory dependencies.

Prefer widely maintained crates and straightforward implementations.

A reasonable module structure would be:

```text
src/
  main.rs
  app.rs
  git.rs
  watcher.rs
  diff.rs
  highlight.rs
  ui.rs
  input.rs
```

Adjust the structure if there is a simpler or more coherent alternative.

### Git integration

Using the system Git executable is acceptable and may be preferable to introducing a complex Git library.

All Git operations should be read-only and use argument arrays rather than shell-constructed command strings.

Handle filenames containing spaces, Unicode characters, and unusual characters correctly. Prefer NUL-delimited Git output for machine parsing.

Account for repository roots, subdirectories, worktrees, and bare-repository errors.

Do not assume the current working directory is the repository root.

### Diff data model

Separate the representation of changes from the presentation modes.

For example, maintain a normalized representation containing:

- File identity and status.
- Old and new paths when applicable.
- Diff hunks.
- Before/after line numbers.
- Added, removed, and context lines.

The unified, side-by-side, and top-bottom renderers should consume a shared underlying representation rather than independently invoke Git with unrelated options.

### Highlighting

Apply syntax highlighting to source code, then combine it with diff-specific formatting.

Diff colors must remain legible against both dark and light terminal themes.

A reasonable default palette is sufficient. Theme configuration is not required for the initial version.

### Live-update architecture

Keep the following concerns separate:

1. Filesystem event detection.
2. Refresh scheduling and debouncing.
3. Git state collection.
4. Diff computation.
5. Rendering.
6. Keyboard and mouse input.

The UI event loop must not block on filesystem scans, large diffs, or Git subprocesses.

Avoid accumulating obsolete refresh requests. If the repository changes during a refresh, schedule another refresh rather than applying stale results over newer data.

Filesystem notifications are hints, not necessarily complete information. Periodic Git reconciliation should ensure eventual correctness.

---

## 5. Command-Line Interface

The following invocations should work:

```bash
gitwatch
gitwatch /path/to/repository
```

Without arguments, find the Git repository containing the current directory.

With a path argument, find the Git repository containing the specified directory.

Initially, no configuration file is necessary.

Optional flags may include:

```text
--quiet-ms <milliseconds>
--max-delay-ms <milliseconds>
--poll-ms <milliseconds>
--help
--version
```

Defaults should work without configuration.

Do not add options unless they serve a concrete requirement.

---

## 6. Edge Cases

Handle the following sensibly:

- A repository with no modifications.
- A repository with no commits.
- Staged and unstaged modifications to the same file.
- Untracked files.
- Deleted files.
- Renamed files.
- Files changed by atomic replacement (temporary file followed by rename).
- Rapid successive saves.
- A commit occurring while the viewer is open.
- The selected file disappearing.
- Binary files.
- Large files.
- Files with very long lines.
- Filenames containing spaces, Unicode, or special characters.
- Terminal resizing.
- Running from a repository subdirectory.
- Running in a Git worktree.
- A repository becoming temporarily unavailable.
- Git errors while a background refresh is running.

Binary files should be identified clearly rather than attempting to display unreadable content as text.

For large files or diffs, prefer graceful limits, lazy processing, or informative warnings over freezing the interface.

Ignore irrelevant filesystem events, especially temporary files and internal `.git` activity, when possible.

Respect Git's ignore rules when identifying untracked files.

Symlinks, submodules, and mode-only changes should be represented sensibly, without requiring full specialized visualizations in the first version.

---

## 7. Read-Only Guarantee

This is an important product requirement.

`gitwatch` must never change repository state.

Specifically, it must not:

- Write or modify project files.
- Modify the Git index.
- Create commits.
- Change branches.
- Create Git references.
- Run staging or reset operations.
- Run cleanup commands.
- Modify `.git/config`.
- Invoke external editors.

Temporary in-memory state is acceptable.

If temporary files are unavoidable, they must reside outside the repository and be cleaned up appropriately.

Inspect Git commands carefully to avoid accidental side effects. In particular, avoid Git operations that may refresh or rewrite the index as a side effect.

The application should work correctly when observing a repository modified concurrently by another process.

The user is responsible for directing the coding agent. `gitwatch` is strictly an observer.

---

## 8. Explicit Non-Goals

Do not implement:

- Git staging or committing.
- Git history browsing.
- Commit review.
- Persistent review sessions.
- Branch management.
- Merge or rebase interfaces.
- Code editing.
- AI integration.
- Multiple agent management.
- IDE features.
- Project management.
- Remote repository hosting integration.
- GitHub API integration.
- Cloud synchronization.
- Web interfaces.
- Background services or daemons.
- Plugins or extensibility frameworks.
- Elaborate configuration systems.

This is **not** intended to compete with Lazygit, GitUI, or full-featured Git clients.

It is a focused live diff viewer.

---

## 9. Testing

Automated tests should cover at least:

1. Correct discovery of changed files.
2. Combination of staged and unstaged changes.
3. Recognition of untracked and deleted files.
4. Removal of committed changes from the display.
5. Diff hunk parsing and alignment.
6. Correct construction of side-by-side rows.
7. Correct construction of top-bottom views.
8. Debounce timer behavior.
9. Maximum refresh delay under continuous changes.
10. Selection and scroll-position preservation.
11. Handling of unusual filenames.
12. Clean handling of Git errors.

Use temporary Git repositories in integration tests.

Test filesystem changes and Git commits occurring while the application is running.

Where possible, keep timing-sensitive tests deterministic using an injectable clock or scheduler rather than relying on long real-time sleeps.

---

## 10. Acceptance Criteria

The implementation is successful when:

- `gitwatch` launches in a Git repository and displays its existing uncommitted changes.
- The changed-file list updates automatically when Codex or Claude Code edits files.
- The diff pane updates automatically after the configured quiet period.
- Continuous editing cannot prevent eventual refresh indefinitely.
- Unified, side-by-side, and top-bottom modes all work.
- Syntax highlighting is visible and appropriate for common source languages.
- The user can navigate changed files and diff hunks entirely with the keyboard.
- Live refreshes do not unnecessarily reset selection or scroll positions.
- When the agent commits, the corresponding changes disappear automatically.
- The application is responsive even during significant filesystem activity.
- Running the application causes no modifications to the repository or Git index.
- The application works in conventional Linux terminal environments, including Byobu and tmux.
- No configuration is required for normal use.

---

## 11. Development Instructions for Codex

Implement this as a complete, usable Rust application, not merely a demonstration or architectural prototype.

Use your judgment for internal implementation details while preserving the specified user-facing behavior.

Prioritize the following order:

1. Reliable Git change discovery.
2. Functional navigable TUI.
3. Correct unified diff display.
4. Live update scheduling.
5. Side-by-side and top-bottom rendering.
6. Syntax highlighting and presentation polish.
7. Edge cases and automated testing.

Do not overengineer the architecture.

Do not introduce features outside the stated scope merely because they are common in other Git tools.

Prefer simple, inspectable code.

Add a concise `README.md` documenting installation, usage, keyboard shortcuts, and any known limitations.

Provide a `Cargo.toml` and ensure the application builds using the stable Rust toolchain on Linux.

Run formatting, linting, and automated tests before declaring the implementation complete.

**Final goal:** A small, fast, dependable, read-only live diff viewer that can be left running in a second terminal while an AI coding agent works in the first.
