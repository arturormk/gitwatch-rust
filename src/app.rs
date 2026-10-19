use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use gitwatch_rust::model::{DiffRow, FileChange, RepositorySnapshot};
use gitwatch_rust::scheduler::{RefreshScheduler, RefreshStatus};
use gitwatch_rust::snapshot::collect_snapshot;
use gitwatch_rust::watcher::{RepositoryWatcher, WatchEvent, WatchSource};
use gitwatch_rust::worker::SnapshotWorker;

use crate::{cli::Cli, highlight::SyntaxHighlighter, terminal::TerminalSession, ui};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Focus {
    Files,
    Diff,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppAction {
    Continue,
    Quit,
    Refresh,
    Shell,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceAnchor {
    hunk: usize,
    old_line: Option<usize>,
    new_line: Option<usize>,
    row_offset: usize,
}

#[derive(Debug)]
pub(crate) struct AppState {
    pub(crate) snapshot: RepositorySnapshot,
    pub(crate) focus: Focus,
    pub(crate) selected: Option<usize>,
    pub(crate) file_offset: usize,
    pub(crate) diff_y: usize,
    pub(crate) diff_x: usize,
    pub(crate) error: Option<String>,
    pub(crate) pending: bool,
    pub(crate) refreshing: bool,
    generation: u64,
}

impl AppState {
    fn new(snapshot: RepositorySnapshot) -> Self {
        let generation = snapshot.generation;
        let selected = (!snapshot.files.is_empty()).then_some(0);
        Self {
            snapshot,
            focus: Focus::Files,
            selected,
            file_offset: 0,
            diff_y: 0,
            diff_x: 0,
            error: None,
            pending: false,
            refreshing: false,
            generation,
        }
    }

    pub(crate) fn selected_file(&self) -> Option<&FileChange> {
        self.selected
            .and_then(|index| self.snapshot.files.get(index))
    }

    fn apply_snapshot(&mut self, snapshot: RepositorySnapshot) {
        let old_index = self.selected.unwrap_or_default();
        let old_key = self.selected_file().map(|file| file.key.clone());
        let anchor = self.current_anchor();

        self.generation = snapshot.generation;
        self.snapshot = snapshot;
        self.error = None;
        self.selected = old_key
            .as_deref()
            .and_then(|key| find_file(&self.snapshot.files, key))
            .or_else(|| {
                (!self.snapshot.files.is_empty())
                    .then_some(old_index.min(self.snapshot.files.len().saturating_sub(1)))
            });
        self.file_offset = self.file_offset.min(self.selected.unwrap_or_default());
        self.diff_y = anchor
            .as_ref()
            .and_then(|anchor| self.selected_file().map(|file| locate_anchor(file, anchor)))
            .unwrap_or(0);
        self.clamp_offsets();
    }

    fn current_anchor(&self) -> Option<SourceAnchor> {
        let file = self.selected_file()?;
        let mut rendered_row = usize::from(file.mode_changed || file.old_path != file.new_path);
        for (hunk_index, hunk) in file.diff.hunks.iter().enumerate() {
            if self.diff_y == rendered_row {
                return Some(SourceAnchor {
                    hunk: hunk_index,
                    old_line: Some(hunk.old_start),
                    new_line: Some(hunk.new_start),
                    row_offset: 0,
                });
            }
            rendered_row += 1;
            for row in &hunk.rows {
                let height = row_height(row);
                if self.diff_y < rendered_row + height {
                    let (old_line, new_line) = row_source_lines(row);
                    return Some(SourceAnchor {
                        hunk: hunk_index,
                        old_line,
                        new_line,
                        row_offset: self.diff_y - rendered_row,
                    });
                }
                rendered_row += height;
            }
        }
        None
    }

    fn move_file(&mut self, amount: isize) {
        if self.snapshot.files.is_empty() {
            return;
        }
        let current = self.selected.unwrap_or_default();
        let next = current
            .saturating_add_signed(amount)
            .min(self.snapshot.files.len() - 1);
        if next != current {
            self.selected = Some(next);
            self.diff_y = 0;
            self.diff_x = 0;
        }
    }

    fn scroll_diff(&mut self, amount: isize) {
        self.diff_y = self.diff_y.saturating_add_signed(amount);
        self.clamp_offsets();
    }

    fn scroll_horizontal(&mut self, amount: isize) {
        self.diff_x = self.diff_x.saturating_add_signed(amount);
    }

    fn move_hunk(&mut self, forward: bool) {
        let starts = self.selected_file().map(hunk_starts).unwrap_or_default();
        if starts.is_empty() {
            return;
        }
        self.diff_y = if forward {
            starts
                .iter()
                .copied()
                .find(|start| *start > self.diff_y)
                .unwrap_or_else(|| *starts.last().expect("hunk starts are not empty"))
        } else {
            starts
                .iter()
                .copied()
                .rev()
                .find(|start| *start < self.diff_y)
                .unwrap_or_else(|| starts[0])
        };
    }

    pub(crate) fn clamp_offsets(&mut self) {
        if self.snapshot.files.is_empty() {
            self.selected = None;
            self.file_offset = 0;
            self.diff_y = 0;
            self.diff_x = 0;
            return;
        }
        self.selected = Some(
            self.selected
                .unwrap_or_default()
                .min(self.snapshot.files.len() - 1),
        );
        self.file_offset = self.file_offset.min(self.snapshot.files.len() - 1);
        self.diff_y = self.diff_y.min(self.max_diff_y());
    }

    fn max_diff_y(&self) -> usize {
        self.selected_file()
            .map(rendered_line_count)
            .unwrap_or_default()
            .saturating_sub(1)
    }
}

pub fn run(terminal: &mut TerminalSession, cli: &Cli) -> Result<()> {
    let snapshot = collect_snapshot(&cli.path, 1)?;
    let (_watcher, watch_events) =
        RepositoryWatcher::start(&snapshot, Duration::from_millis(cli.poll_ms))?;
    let worker = SnapshotWorker::spawn(cli.path.clone());
    let mut scheduler = RefreshScheduler::new(
        Duration::from_millis(cli.quiet_ms),
        Duration::from_millis(cli.max_delay_ms),
    );
    let mut state = AppState::new(snapshot);
    let mut next_generation = state.generation;
    let highlighter = SyntaxHighlighter::new();
    let mut redraw = true;

    // Close the small startup window between collecting the initial snapshot
    // and establishing the filesystem watches.
    scheduler.manual_refresh();
    request_snapshot(&worker, &mut state, &mut next_generation)?;

    loop {
        let previous_status = scheduler.status();
        for event in watch_events.try_iter() {
            match event {
                WatchEvent::Change(WatchSource::Filesystem) => {
                    scheduler.filesystem_hint(Instant::now());
                }
                WatchEvent::Change(WatchSource::Polling) => {
                    scheduler.polling_hint(Instant::now());
                }
                WatchEvent::Error(error) => {
                    redraw |= state.error.as_deref() != Some(error.as_str());
                    state.error = Some(error);
                }
            }
        }

        while let Some(result) = worker.try_result().map_err(anyhow::Error::msg)? {
            let mut failed = false;
            if result.generation >= state.generation {
                match result.snapshot {
                    Ok(snapshot) => state.apply_snapshot(snapshot),
                    Err(error) => {
                        state.error = Some(error);
                        failed = true;
                    }
                }
            }
            if scheduler.refresh_finished() {
                request_snapshot(&worker, &mut state, &mut next_generation)?;
            } else if failed {
                scheduler.polling_hint(Instant::now());
            }
            redraw = true;
        }

        if scheduler.tick(Instant::now()) {
            request_snapshot(&worker, &mut state, &mut next_generation)?;
            redraw = true;
        }
        sync_refresh_status(&scheduler, &mut state);
        redraw |= scheduler.status() != previous_status;

        if redraw {
            terminal
                .terminal_mut()
                .draw(|frame| ui::render(frame, &mut state, &highlighter))?;
            redraw = false;
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match handle_key(key, &mut state) {
                    AppAction::Quit => return Ok(()),
                    AppAction::Shell => {
                        terminal.run_shell(&state.snapshot.repository_root)?;
                        redraw = true;
                    }
                    AppAction::Refresh => {
                        if scheduler.manual_refresh() {
                            request_snapshot(&worker, &mut state, &mut next_generation)?;
                        }
                        sync_refresh_status(&scheduler, &mut state);
                        redraw = true;
                    }
                    AppAction::Continue => redraw = true,
                }
            }
            Event::Mouse(mouse) => {
                let size = terminal.terminal_mut().size()?;
                let area = Rect::new(0, 0, size.width, size.height);
                redraw |= handle_mouse(mouse, &mut state, area);
            }
            Event::Resize(_, _) => {
                state.clamp_offsets();
                redraw = true;
            }
            _ => {}
        }
    }
}

fn request_snapshot(
    worker: &SnapshotWorker,
    state: &mut AppState,
    next_generation: &mut u64,
) -> Result<()> {
    *next_generation = next_generation.saturating_add(1);
    worker
        .request(*next_generation)
        .map_err(anyhow::Error::msg)?;
    state.pending = false;
    state.refreshing = true;
    Ok(())
}

fn sync_refresh_status(scheduler: &RefreshScheduler, state: &mut AppState) {
    state.pending = scheduler.status() == RefreshStatus::Pending;
    state.refreshing = scheduler.status() == RefreshStatus::Refreshing;
}

fn handle_mouse(mouse: MouseEvent, state: &mut AppState, area: Rect) -> bool {
    if state.snapshot.files.is_empty() {
        return false;
    }
    let before = (state.focus, state.selected, state.diff_y, state.diff_x);
    let (files, diff) = ui::pane_areas(area);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) if in_inner(files, mouse.column, mouse.row) => {
            let visible_row = usize::from(mouse.row - files.y - 1);
            let index = state.file_offset + visible_row;
            if index < state.snapshot.files.len() {
                if state.selected != Some(index) {
                    state.selected = Some(index);
                    state.diff_y = 0;
                    state.diff_x = 0;
                }
                state.focus = Focus::Files;
            }
        }
        MouseEventKind::ScrollUp if contains(diff, mouse.column, mouse.row) => {
            state.scroll_diff(-3);
        }
        MouseEventKind::ScrollDown if contains(diff, mouse.column, mouse.row) => {
            state.scroll_diff(3);
        }
        _ => {}
    }
    before != (state.focus, state.selected, state.diff_y, state.diff_x)
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

fn in_inner(area: Rect, column: u16, row: u16) -> bool {
    area.width > 2
        && area.height > 2
        && column > area.x
        && column < area.x.saturating_add(area.width).saturating_sub(1)
        && row > area.y
        && row < area.y.saturating_add(area.height).saturating_sub(1)
}

fn handle_key(key: KeyEvent, state: &mut AppState) -> AppAction {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return AppAction::Quit,
        KeyCode::Char('r') => return AppAction::Refresh,
        KeyCode::Char('t') => return AppAction::Shell,
        KeyCode::Tab => {
            state.focus = match state.focus {
                Focus::Files => Focus::Diff,
                Focus::Diff => Focus::Files,
            };
        }
        KeyCode::Up | KeyCode::Char('k') => match state.focus {
            Focus::Files => state.move_file(-1),
            Focus::Diff => state.scroll_diff(-1),
        },
        KeyCode::Down | KeyCode::Char('j') => match state.focus {
            Focus::Files => state.move_file(1),
            Focus::Diff => state.scroll_diff(1),
        },
        KeyCode::PageUp => match state.focus {
            Focus::Files => state.move_file(-10),
            Focus::Diff => state.scroll_diff(-20),
        },
        KeyCode::PageDown => match state.focus {
            Focus::Files => state.move_file(10),
            Focus::Diff => state.scroll_diff(20),
        },
        KeyCode::Home => match state.focus {
            Focus::Files => state.move_file(-(state.snapshot.files.len() as isize)),
            Focus::Diff => state.diff_y = 0,
        },
        KeyCode::End => match state.focus {
            Focus::Files => state.move_file(state.snapshot.files.len() as isize),
            Focus::Diff => state.diff_y = state.max_diff_y(),
        },
        KeyCode::Left | KeyCode::Char('h') if state.focus == Focus::Diff => {
            state.scroll_horizontal(-4);
        }
        KeyCode::Right | KeyCode::Char('l') if state.focus == Focus::Diff => {
            state.scroll_horizontal(4);
        }
        KeyCode::Char('n') => state.move_hunk(true),
        KeyCode::Char('p') => state.move_hunk(false),
        _ => {}
    }
    AppAction::Continue
}

fn find_file(files: &[FileChange], old_key: &Path) -> Option<usize> {
    files.iter().position(|file| {
        file.key == old_key
            || file.old_path.as_deref() == Some(old_key)
            || file.new_path.as_deref() == Some(old_key)
    })
}

pub(crate) fn row_height(row: &DiffRow) -> usize {
    usize::from(matches!(row, DiffRow::Replace { .. })) + 1
}

fn row_source_lines(row: &DiffRow) -> (Option<usize>, Option<usize>) {
    match row {
        DiffRow::Equal {
            old_line, new_line, ..
        }
        | DiffRow::Replace {
            old_line, new_line, ..
        } => (Some(*old_line), Some(*new_line)),
        DiffRow::Delete { old_line, .. } => (Some(*old_line), None),
        DiffRow::Insert { new_line, .. } => (None, Some(*new_line)),
    }
}

pub(crate) fn rendered_line_count(file: &FileChange) -> usize {
    usize::from(file.mode_changed || file.old_path != file.new_path)
        + file
            .diff
            .hunks
            .iter()
            .map(|hunk| 1 + hunk.rows.iter().map(row_height).sum::<usize>())
            .sum::<usize>()
}

pub(crate) fn hunk_starts(file: &FileChange) -> Vec<usize> {
    let mut offset = usize::from(file.mode_changed || file.old_path != file.new_path);
    file.diff
        .hunks
        .iter()
        .map(|hunk| {
            let start = offset;
            offset += 1 + hunk.rows.iter().map(row_height).sum::<usize>();
            start
        })
        .collect()
}

fn locate_anchor(file: &FileChange, anchor: &SourceAnchor) -> usize {
    let starts = hunk_starts(file);
    let hunk_index = anchor.hunk.min(file.diff.hunks.len().saturating_sub(1));
    let Some(hunk) = file.diff.hunks.get(hunk_index) else {
        return 0;
    };
    let mut offset = starts.get(hunk_index).copied().unwrap_or_default() + 1;
    let mut best = (usize::MAX, offset);
    for row in &hunk.rows {
        let (old_line, new_line) = row_source_lines(row);
        let distance = old_line
            .zip(anchor.old_line)
            .map(|(line, wanted)| line.abs_diff(wanted))
            .or_else(|| {
                new_line
                    .zip(anchor.new_line)
                    .map(|(line, wanted)| line.abs_diff(wanted))
            })
            .unwrap_or(usize::MAX);
        if distance < best.0 {
            best = (
                distance,
                offset + anchor.row_offset.min(row_height(row) - 1),
            );
        }
        offset += row_height(row);
    }
    best.1
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::SystemTime};

    use gitwatch_rust::model::{ChangeStatus, DiffHunk, FileDiff, FileKind};

    use super::*;

    fn file(path: &str, line: usize) -> FileChange {
        FileChange {
            key: PathBuf::from(path),
            old_path: Some(PathBuf::from(path)),
            new_path: Some(PathBuf::from(path)),
            status: ChangeStatus::Modified,
            staged: false,
            unstaged: true,
            kind: FileKind::Text,
            old_content: None,
            new_content: None,
            old_mode: Some(0o100644),
            new_mode: Some(0o100644),
            mode_changed: false,
            diff: FileDiff {
                hunks: vec![DiffHunk {
                    old_start: line,
                    old_len: 1,
                    new_start: line,
                    new_len: 1,
                    rows: vec![DiffRow::Replace {
                        old_line: line,
                        old_content: "old".into(),
                        new_line: line,
                        new_content: "new".into(),
                    }],
                }],
            },
        }
    }

    fn snapshot(generation: u64, files: Vec<FileChange>) -> RepositorySnapshot {
        RepositorySnapshot {
            generation,
            repository_root: PathBuf::from("/repo"),
            git_dir: PathBuf::from("/repo/.git"),
            common_git_dir: PathBuf::from("/repo/.git"),
            head_identity: Some("head".into()),
            collected_at: SystemTime::now(),
            files,
            warnings: vec![],
        }
    }

    #[test]
    fn refresh_preserves_selected_key_and_source_anchor() {
        let mut state = AppState::new(snapshot(1, vec![file("a", 10), file("b", 20)]));
        state.selected = Some(1);
        state.diff_y = 2;
        state.apply_snapshot(snapshot(2, vec![file("b", 20), file("c", 30)]));
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.selected_file().unwrap().key, PathBuf::from("b"));
        assert_eq!(state.diff_y, 2);
    }

    #[test]
    fn refresh_uses_nearest_index_then_handles_clean_tree() {
        let mut state = AppState::new(snapshot(1, vec![file("a", 1), file("b", 1)]));
        state.selected = Some(1);
        state.apply_snapshot(snapshot(2, vec![file("a", 1)]));
        assert_eq!(state.selected, Some(0));
        state.apply_snapshot(snapshot(3, vec![]));
        assert_eq!(state.selected, None);
        assert_eq!(state.diff_y, 0);
    }

    #[test]
    fn hunk_navigation_uses_rendered_row_offsets() {
        let mut changed = file("a", 1);
        changed.diff.hunks.push(changed.diff.hunks[0].clone());
        let mut state = AppState::new(snapshot(1, vec![changed]));
        state.move_hunk(true);
        assert_eq!(state.diff_y, 3);
        state.move_hunk(true);
        assert_eq!(state.diff_y, 3);
        state.move_hunk(false);
        assert_eq!(state.diff_y, 0);
    }

    #[test]
    fn terminal_key_requests_an_interactive_shell() {
        let mut state = AppState::new(snapshot(1, vec![]));
        let key = KeyEvent::new(KeyCode::Char('t'), crossterm::event::KeyModifiers::NONE);
        assert_eq!(handle_key(key, &mut state), AppAction::Shell);
    }

    #[test]
    fn mouse_click_selects_visible_file_using_list_offset() {
        let mut state = AppState::new(snapshot(1, vec![file("a", 1), file("b", 1), file("c", 1)]));
        state.file_offset = 1;
        state.diff_y = 2;
        let area = Rect::new(0, 0, 100, 30);
        let (files, _) = ui::pane_areas(area);
        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: files.x + 1,
                row: files.y + 2,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            &mut state,
            area,
        );
        assert_eq!(state.selected, Some(2));
        assert_eq!(state.diff_y, 0);
    }

    #[test]
    fn mouse_wheel_scrolls_only_over_diff_pane() {
        let mut state = AppState::new(snapshot(1, vec![file("a", 1)]));
        let area = Rect::new(0, 0, 100, 30);
        let (files, diff) = ui::pane_areas(area);
        let wheel = |kind, pane: Rect| MouseEvent {
            kind,
            column: pane.x + 1,
            row: pane.y + 1,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };

        handle_mouse(wheel(MouseEventKind::ScrollDown, files), &mut state, area);
        assert_eq!(state.diff_y, 0);
        handle_mouse(wheel(MouseEventKind::ScrollDown, diff), &mut state, area);
        assert_eq!(state.diff_y, 2);
        handle_mouse(wheel(MouseEventKind::ScrollUp, diff), &mut state, area);
        assert_eq!(state.diff_y, 0);
    }
}
