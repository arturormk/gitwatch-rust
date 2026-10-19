use std::io;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::{Terminal, backend::CrosstermBackend};

use gitwatch_rust::model::{DiffRow, FileChange, RepositorySnapshot};
use gitwatch_rust::snapshot::collect_snapshot;

use crate::{cli::Cli, highlight::SyntaxHighlighter, ui};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Focus {
    Files,
    Diff,
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

pub fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, cli: &Cli) -> Result<()> {
    let snapshot = collect_snapshot(&cli.path, 1)?;
    let mut state = AppState::new(snapshot);
    let highlighter = SyntaxHighlighter::new();

    loop {
        terminal.draw(|frame| ui::render(frame, &mut state, &highlighter))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if handle_key(key, &mut state) {
                    return Ok(());
                }
                if matches!(key.code, KeyCode::Char('r')) {
                    state.refreshing = true;
                    terminal.draw(|frame| ui::render(frame, &mut state, &highlighter))?;
                    let next_generation = state.generation + 1;
                    match collect_snapshot(&cli.path, next_generation) {
                        Ok(snapshot) => state.apply_snapshot(snapshot),
                        Err(error) => state.error = Some(format!("{error:#}")),
                    }
                    state.refreshing = false;
                }
            }
            Event::Resize(_, _) => state.clamp_offsets(),
            _ => {}
        }
    }
}

fn handle_key(key: KeyEvent, state: &mut AppState) -> bool {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
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
    false
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
}
