use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use gitwatch_rust::document::{
    DocumentContent, DocumentationSnapshot, MarkdownDocument, collect_documents,
};
use gitwatch_rust::model::{DiffRow, FileChange, RepositorySnapshot};
use gitwatch_rust::scheduler::{RefreshScheduler, RefreshStatus};
use gitwatch_rust::snapshot::collect_snapshot;
use gitwatch_rust::watcher::{RepositoryWatcher, WatchEvent, WatchSource};
use gitwatch_rust::worker::SnapshotWorker;

use crate::{
    cli::Cli, highlight::SyntaxHighlighter, markdown::MarkdownCache, terminal::TerminalSession, ui,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Focus {
    Files,
    Diff,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ViewMode {
    Diff,
    Documentation,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SearchMatch {
    line: usize,
    column: usize,
}

#[derive(Debug, Default)]
struct SearchState {
    editing: bool,
    input: String,
    query: String,
    lines: Vec<String>,
    matches: Vec<SearchMatch>,
    current: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct AppState {
    pub(crate) snapshot: RepositorySnapshot,
    pub(crate) documentation: DocumentationSnapshot,
    pub(crate) view_mode: ViewMode,
    pub(crate) focus: Focus,
    pub(crate) selected: Option<usize>,
    pub(crate) file_offset: usize,
    pub(crate) diff_y: usize,
    pub(crate) diff_x: usize,
    pub(crate) document_selected: Option<usize>,
    pub(crate) document_offset: usize,
    pub(crate) document_y: usize,
    pub(crate) document_x: usize,
    document_positions: HashMap<PathBuf, (usize, usize)>,
    pub(crate) markdown_cache: MarkdownCache,
    search: SearchState,
    pub(crate) error: Option<String>,
    pub(crate) pending: bool,
    pub(crate) refreshing: bool,
    generation: u64,
}

impl AppState {
    fn new(snapshot: RepositorySnapshot, documentation: DocumentationSnapshot) -> Self {
        let generation = snapshot.generation;
        let selected = (!snapshot.files.is_empty()).then_some(0);
        let document_selected = (!documentation.documents.is_empty()).then_some(0);
        Self {
            snapshot,
            documentation,
            view_mode: ViewMode::Diff,
            focus: Focus::Files,
            selected,
            file_offset: 0,
            diff_y: 0,
            diff_x: 0,
            document_selected,
            document_offset: 0,
            document_y: 0,
            document_x: 0,
            document_positions: HashMap::new(),
            markdown_cache: MarkdownCache::default(),
            search: SearchState::default(),
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

    pub(crate) fn selected_document(&self) -> Option<&MarkdownDocument> {
        self.document_selected
            .and_then(|index| self.documentation.documents.get(index))
    }

    pub(crate) fn active_file_count(&self) -> usize {
        match self.view_mode {
            ViewMode::Diff => self.snapshot.files.len(),
            ViewMode::Documentation => self.documentation.documents.len(),
        }
    }

    pub(crate) fn active_file_offset(&self) -> usize {
        match self.view_mode {
            ViewMode::Diff => self.file_offset,
            ViewMode::Documentation => self.document_offset,
        }
    }

    fn apply_documentation(&mut self, mut documentation: DocumentationSnapshot) {
        let old_index = self.document_selected.unwrap_or_default();
        let old_path = self
            .selected_document()
            .map(|document| document.path.clone());
        self.remember_document_position();
        for document in &mut documentation.documents {
            let DocumentContent::Unavailable(error) = &document.content else {
                continue;
            };
            let Some(previous) = self
                .documentation
                .documents
                .iter()
                .find(|previous| previous.path == document.path)
            else {
                continue;
            };
            if !matches!(previous.content, DocumentContent::Unavailable(_)) {
                documentation.warnings.push(format!(
                    "could not refresh {}: {error}; showing the last good content",
                    document.path.display()
                ));
                document.content = previous.content.clone();
            }
        }
        self.documentation = documentation;
        self.document_selected = old_path
            .as_deref()
            .and_then(|path| {
                self.documentation
                    .documents
                    .iter()
                    .position(|document| document.path == path)
            })
            .or_else(|| {
                (!self.documentation.documents.is_empty())
                    .then_some(old_index.min(self.documentation.documents.len().saturating_sub(1)))
            });
        self.document_offset = self
            .document_offset
            .min(self.document_selected.unwrap_or_default());
        if self.document_selected.is_none() {
            self.document_y = 0;
            self.document_x = 0;
        } else if self.selected_document().map(|document| &document.path) != old_path.as_ref() {
            self.restore_document_position();
        }
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

    fn move_document(&mut self, amount: isize) {
        if self.documentation.documents.is_empty() {
            return;
        }
        let current = self.document_selected.unwrap_or_default();
        let next = current
            .saturating_add_signed(amount)
            .min(self.documentation.documents.len() - 1);
        if next != current {
            self.select_document(next);
        }
    }

    fn select_document(&mut self, index: usize) {
        self.remember_document_position();
        self.document_selected = Some(index.min(self.documentation.documents.len() - 1));
        self.restore_document_position();
    }

    fn remember_document_position(&mut self) {
        if let Some(path) = self
            .selected_document()
            .map(|document| document.path.clone())
        {
            self.document_positions
                .insert(path, (self.document_y, self.document_x));
        }
    }

    fn restore_document_position(&mut self) {
        let position = self
            .selected_document()
            .and_then(|document| self.document_positions.get(&document.path))
            .copied()
            .unwrap_or_default();
        (self.document_y, self.document_x) = position;
    }

    fn move_active_file(&mut self, amount: isize) {
        match self.view_mode {
            ViewMode::Diff => self.move_file(amount),
            ViewMode::Documentation => self.move_document(amount),
        }
    }

    fn scroll_diff(&mut self, amount: isize) {
        self.diff_y = self.diff_y.saturating_add_signed(amount);
        self.clamp_offsets();
    }

    fn scroll_content(&mut self, amount: isize) {
        match self.view_mode {
            ViewMode::Diff => self.scroll_diff(amount),
            ViewMode::Documentation => {
                self.document_y = self.document_y.saturating_add_signed(amount);
            }
        }
    }

    fn scroll_horizontal(&mut self, amount: isize) {
        match self.view_mode {
            ViewMode::Diff => self.diff_x = self.diff_x.saturating_add_signed(amount),
            ViewMode::Documentation => {
                self.document_x = self.document_x.saturating_add_signed(amount);
            }
        }
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
        } else {
            self.selected = Some(
                self.selected
                    .unwrap_or_default()
                    .min(self.snapshot.files.len() - 1),
            );
            self.file_offset = self.file_offset.min(self.snapshot.files.len() - 1);
            self.diff_y = self.diff_y.min(self.max_diff_y());
        }
        if self.documentation.documents.is_empty() {
            self.document_selected = None;
            self.document_offset = 0;
            self.document_y = 0;
            self.document_x = 0;
        } else {
            self.document_selected = Some(
                self.document_selected
                    .unwrap_or_default()
                    .min(self.documentation.documents.len() - 1),
            );
            self.document_offset = self
                .document_offset
                .min(self.documentation.documents.len() - 1);
        }
    }

    fn max_diff_y(&self) -> usize {
        self.selected_file()
            .map(rendered_line_count)
            .unwrap_or_default()
            .saturating_sub(1)
    }

    pub(crate) fn update_search_content(&mut self, lines: Vec<String>) {
        if self.search.lines != lines {
            self.search.lines = lines;
            self.search.current = None;
            self.rebuild_search_matches();
        }
    }

    pub(crate) fn search_query(&self) -> &str {
        &self.search.query
    }

    pub(crate) fn search_prompt(&self) -> Option<&str> {
        self.search.editing.then_some(self.search.input.as_str())
    }

    pub(crate) fn search_status(&self) -> Option<String> {
        if self.search.editing || self.search.query.is_empty() {
            return None;
        }
        let position = self.search.current.map_or(0, |index| index + 1);
        Some(format!(
            "/{}  {position}/{}",
            self.search.query,
            self.search.matches.len()
        ))
    }

    fn begin_search(&mut self) {
        self.focus = Focus::Diff;
        self.search.editing = true;
        self.search.input.clear();
    }

    fn clear_search(&mut self) {
        self.search.editing = false;
        self.search.input.clear();
        self.search.query.clear();
        self.search.matches.clear();
        self.search.current = None;
    }

    fn rebuild_search_matches(&mut self) {
        self.search.matches.clear();
        if self.search.query.is_empty() {
            return;
        }
        for (line, text) in self.search.lines.iter().enumerate() {
            self.search
                .matches
                .extend(
                    text.match_indices(&self.search.query)
                        .map(|(column, _)| SearchMatch {
                            line,
                            column: UnicodeWidthStr::width(&text[..column]),
                        }),
                );
        }
        if self
            .search
            .current
            .is_some_and(|index| index >= self.search.matches.len())
        {
            self.search.current = None;
        }
    }

    fn finish_search(&mut self) {
        self.search.editing = false;
        if !self.search.input.is_empty() {
            self.search.query.clone_from(&self.search.input);
            self.search.current = None;
            self.rebuild_search_matches();
        }
        self.move_search(true, true);
    }

    fn move_search(&mut self, forward: bool, include_viewport: bool) {
        if self.search.matches.is_empty() {
            self.search.current = None;
            return;
        }
        let viewport = match self.view_mode {
            ViewMode::Diff => self.diff_y,
            ViewMode::Documentation => self.document_y,
        };
        let next = if let Some(current) = self.search.current {
            if forward {
                (current + 1) % self.search.matches.len()
            } else {
                current
                    .checked_sub(1)
                    .unwrap_or(self.search.matches.len() - 1)
            }
        } else if forward {
            self.search
                .matches
                .iter()
                .position(|found| {
                    found.line > viewport || (include_viewport && found.line == viewport)
                })
                .unwrap_or(0)
        } else {
            self.search
                .matches
                .iter()
                .rposition(|found| {
                    found.line < viewport || (include_viewport && found.line == viewport)
                })
                .unwrap_or(self.search.matches.len() - 1)
        };
        self.search.current = Some(next);
        let found = self.search.matches[next];
        match self.view_mode {
            ViewMode::Diff => {
                self.diff_y = found.line;
                self.diff_x = found.column.saturating_sub(4);
            }
            ViewMode::Documentation => {
                self.document_y = found.line;
                self.document_x = found.column.saturating_sub(4);
            }
        }
    }
}

pub fn run(terminal: &mut TerminalSession, cli: &Cli) -> Result<()> {
    let snapshot = collect_snapshot(&cli.path, 1)?;
    let documentation = collect_documents(&snapshot)?;
    let (_watcher, watch_events) =
        RepositoryWatcher::start(&snapshot, Duration::from_millis(cli.poll_ms))?;
    let worker = SnapshotWorker::spawn(cli.path.clone());
    let mut scheduler = RefreshScheduler::new(
        Duration::from_millis(cli.quiet_ms),
        Duration::from_millis(cli.max_delay_ms),
    );
    let mut state = AppState::new(snapshot, documentation);
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
                match result.documentation {
                    Ok(documentation) => state.apply_documentation(documentation),
                    Err(error) if !failed => {
                        state.error = Some(format!("documentation refresh failed: {error}"));
                    }
                    Err(_) => {}
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
    let before = (
        state.focus,
        state.selected,
        state.diff_y,
        state.diff_x,
        state.document_selected,
        state.document_y,
        state.document_x,
    );
    let (files, diff) = ui::pane_areas(area);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) if contains(files, mouse.column, mouse.row) => {
            state.focus = Focus::Files;
            if in_inner(files, mouse.column, mouse.row) {
                let visible_row = usize::from(mouse.row - files.y - 1);
                let index = state.active_file_offset() + visible_row;
                if index < state.active_file_count() {
                    match state.view_mode {
                        ViewMode::Diff if state.selected != Some(index) => {
                            state.selected = Some(index);
                            state.diff_y = 0;
                            state.diff_x = 0;
                        }
                        ViewMode::Documentation if state.document_selected != Some(index) => {
                            state.select_document(index);
                        }
                        _ => {}
                    }
                }
            }
        }
        MouseEventKind::Down(MouseButton::Left) if contains(diff, mouse.column, mouse.row) => {
            state.focus = Focus::Diff;
        }
        MouseEventKind::ScrollUp if contains(diff, mouse.column, mouse.row) => {
            state.scroll_content(-3);
        }
        MouseEventKind::ScrollDown if contains(diff, mouse.column, mouse.row) => {
            state.scroll_content(3);
        }
        _ => {}
    }
    before
        != (
            state.focus,
            state.selected,
            state.diff_y,
            state.diff_x,
            state.document_selected,
            state.document_y,
            state.document_x,
        )
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
    if state.search.editing {
        match key.code {
            KeyCode::Esc => state.search.editing = false,
            KeyCode::Enter => state.finish_search(),
            KeyCode::Backspace => {
                state.search.input.pop();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                state.search.input.push(character);
            }
            _ => {}
        }
        return AppAction::Continue;
    }
    match key.code {
        KeyCode::Esc if !state.search.query.is_empty() => {
            state.clear_search();
        }
        KeyCode::Char('q') | KeyCode::Esc => return AppAction::Quit,
        KeyCode::Char('r') => return AppAction::Refresh,
        KeyCode::Char('t') => return AppAction::Shell,
        KeyCode::Char('d') => {
            state.view_mode = match state.view_mode {
                ViewMode::Diff => ViewMode::Documentation,
                ViewMode::Documentation => ViewMode::Diff,
            };
        }
        KeyCode::Char('/') => state.begin_search(),
        KeyCode::Tab => {
            state.focus = match state.focus {
                Focus::Files => Focus::Diff,
                Focus::Diff => Focus::Files,
            };
        }
        KeyCode::Up | KeyCode::Char('k') => match state.focus {
            Focus::Files => state.move_active_file(-1),
            Focus::Diff => state.scroll_content(-1),
        },
        KeyCode::Down | KeyCode::Char('j') => match state.focus {
            Focus::Files => state.move_active_file(1),
            Focus::Diff => state.scroll_content(1),
        },
        KeyCode::PageUp => match state.focus {
            Focus::Files => state.move_active_file(-10),
            Focus::Diff => state.scroll_content(-20),
        },
        KeyCode::PageDown => match state.focus {
            Focus::Files => state.move_active_file(10),
            Focus::Diff => state.scroll_content(20),
        },
        KeyCode::Home => match state.focus {
            Focus::Files => state.move_active_file(-(state.active_file_count() as isize)),
            Focus::Diff => match state.view_mode {
                ViewMode::Diff => state.diff_y = 0,
                ViewMode::Documentation => state.document_y = 0,
            },
        },
        KeyCode::End => match state.focus {
            Focus::Files => state.move_active_file(state.active_file_count() as isize),
            Focus::Diff => match state.view_mode {
                ViewMode::Diff => state.diff_y = state.max_diff_y(),
                ViewMode::Documentation => state.document_y = usize::MAX,
            },
        },
        KeyCode::Left | KeyCode::Char('h') if state.focus == Focus::Diff => {
            state.scroll_horizontal(-4);
        }
        KeyCode::Right | KeyCode::Char('l') if state.focus == Focus::Diff => {
            state.scroll_horizontal(4);
        }
        KeyCode::Char('n') if !state.search.query.is_empty() => state.move_search(true, false),
        KeyCode::Char('N') if !state.search.query.is_empty() => state.move_search(false, false),
        KeyCode::Char('n') if state.view_mode == ViewMode::Diff => state.move_hunk(true),
        KeyCode::Char('p') if state.view_mode == ViewMode::Diff => state.move_hunk(false),
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

    fn state(generation: u64, files: Vec<FileChange>) -> AppState {
        AppState::new(
            snapshot(generation, files),
            DocumentationSnapshot::default(),
        )
    }

    fn documentation(paths: &[(&str, &str)]) -> DocumentationSnapshot {
        DocumentationSnapshot {
            documents: paths
                .iter()
                .map(|(path, content)| MarkdownDocument {
                    path: PathBuf::from(path),
                    content: DocumentContent::Text((*content).into()),
                })
                .collect(),
            warnings: vec![],
            incomplete: false,
        }
    }

    #[test]
    fn refresh_preserves_selected_key_and_source_anchor() {
        let mut state = state(1, vec![file("a", 10), file("b", 20)]);
        state.selected = Some(1);
        state.diff_y = 2;
        state.apply_snapshot(snapshot(2, vec![file("b", 20), file("c", 30)]));
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.selected_file().unwrap().key, PathBuf::from("b"));
        assert_eq!(state.diff_y, 2);
    }

    #[test]
    fn refresh_uses_nearest_index_then_handles_clean_tree() {
        let mut state = state(1, vec![file("a", 1), file("b", 1)]);
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
        let mut state = state(1, vec![changed]);
        state.move_hunk(true);
        assert_eq!(state.diff_y, 3);
        state.move_hunk(true);
        assert_eq!(state.diff_y, 3);
        state.move_hunk(false);
        assert_eq!(state.diff_y, 0);
    }

    #[test]
    fn terminal_key_requests_an_interactive_shell() {
        let mut state = state(1, vec![]);
        let key = KeyEvent::new(KeyCode::Char('t'), crossterm::event::KeyModifiers::NONE);
        assert_eq!(handle_key(key, &mut state), AppAction::Shell);
    }

    #[test]
    fn slash_focuses_content_and_searches_rendered_lines() {
        let mut state = state(1, vec![file("a", 1)]);
        state.update_search_content(vec![
            "first line".into(),
            "a line with needle far enough across".into(),
            "last needle".into(),
        ]);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        assert_eq!(
            handle_key(key(KeyCode::Char('/')), &mut state),
            AppAction::Continue
        );
        assert_eq!(state.focus, Focus::Diff);
        assert_eq!(state.search_prompt(), Some(""));
        for character in "needle".chars() {
            handle_key(key(KeyCode::Char(character)), &mut state);
        }
        handle_key(key(KeyCode::Enter), &mut state);

        assert_eq!(state.search_prompt(), None);
        assert_eq!(state.search_status().as_deref(), Some("/needle  1/2"));
        assert_eq!(state.diff_y, 1);
        assert!(state.diff_x > 0);

        handle_key(key(KeyCode::Char('n')), &mut state);
        assert_eq!(state.diff_y, 2);
        handle_key(key(KeyCode::Char('N')), &mut state);
        assert_eq!(state.diff_y, 1);
        assert_eq!(
            handle_key(key(KeyCode::Esc), &mut state),
            AppAction::Continue
        );
        assert_eq!(state.search_status(), None);
    }

    #[test]
    fn search_input_captures_commands_and_escape_only_cancels_input() {
        let mut state = state(1, vec![file("a", 1)]);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        handle_key(key(KeyCode::Char('/')), &mut state);
        assert_eq!(
            handle_key(key(KeyCode::Char('q')), &mut state),
            AppAction::Continue
        );
        assert_eq!(state.search_prompt(), Some("q"));
        assert_eq!(
            handle_key(key(KeyCode::Esc), &mut state),
            AppAction::Continue
        );
        assert_eq!(state.search_prompt(), None);
    }

    #[test]
    fn documentation_key_toggles_without_losing_diff_or_document_state() {
        let mut state = AppState::new(
            snapshot(1, vec![file("source.rs", 1)]),
            documentation(&[("README.md", "# One"), ("docs/plan.md", "# Plan")]),
        );
        state.diff_y = 2;
        let key = KeyEvent::new(KeyCode::Char('d'), crossterm::event::KeyModifiers::NONE);
        handle_key(key, &mut state);
        assert_eq!(state.view_mode, ViewMode::Documentation);
        state.move_document(1);
        state.document_y = 4;
        handle_key(key, &mut state);
        assert_eq!(state.view_mode, ViewMode::Diff);
        assert_eq!(state.diff_y, 2);
        handle_key(key, &mut state);
        assert_eq!(
            state.selected_document().unwrap().path,
            PathBuf::from("docs/plan.md")
        );
        assert_eq!(state.document_y, 4);
    }

    #[test]
    fn documentation_refresh_preserves_open_path_and_reading_position() {
        let mut state = AppState::new(
            snapshot(1, vec![]),
            documentation(&[("README.md", "old"), ("docs/plan.md", "plan")]),
        );
        state.document_selected = Some(0);
        state.document_y = 7;
        state.apply_documentation(documentation(&[
            ("README.md", "new on-disk contents"),
            ("docs/other.md", "other"),
        ]));
        assert_eq!(
            state.selected_document().unwrap().path,
            PathBuf::from("README.md")
        );
        assert_eq!(state.document_y, 7);
        assert_eq!(
            state.selected_document().unwrap().content,
            DocumentContent::Text("new on-disk contents".into())
        );
    }

    #[test]
    fn documentation_refresh_retains_last_good_content_after_read_error() {
        let mut state = AppState::new(
            snapshot(1, vec![]),
            documentation(&[("README.md", "last good content")]),
        );
        state.apply_documentation(DocumentationSnapshot {
            documents: vec![MarkdownDocument {
                path: PathBuf::from("README.md"),
                content: DocumentContent::Unavailable("permission denied".into()),
            }],
            warnings: vec![],
            incomplete: false,
        });
        assert_eq!(
            state.selected_document().unwrap().content,
            DocumentContent::Text("last good content".into())
        );
        assert!(state.documentation.warnings[0].contains("permission denied"));
    }

    #[test]
    fn switching_documents_restores_each_reading_position() {
        let mut state = AppState::new(
            snapshot(1, vec![]),
            documentation(&[("README.md", "readme"), ("docs/plan.md", "plan")]),
        );
        state.document_y = 8;
        state.document_x = 3;
        state.move_document(1);
        assert_eq!((state.document_y, state.document_x), (0, 0));
        state.document_y = 21;
        state.move_document(-1);
        assert_eq!((state.document_y, state.document_x), (8, 3));
        state.move_document(1);
        assert_eq!((state.document_y, state.document_x), (21, 0));
    }

    #[test]
    fn mouse_click_selects_visible_file_using_list_offset() {
        let mut state = state(1, vec![file("a", 1), file("b", 1), file("c", 1)]);
        state.focus = Focus::Diff;
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
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(state.selected, Some(2));
        assert_eq!(state.diff_y, 0);
    }

    #[test]
    fn mouse_click_focuses_either_pane_without_changing_file_selection() {
        let mut state = state(1, vec![file("a", 1), file("b", 1)]);
        state.selected = Some(1);
        let area = Rect::new(0, 0, 100, 30);
        let (files, diff) = ui::pane_areas(area);
        let click = |pane: Rect| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.x,
            row: pane.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };

        handle_mouse(click(diff), &mut state, area);
        assert_eq!(state.focus, Focus::Diff);
        assert_eq!(state.selected, Some(1));

        handle_mouse(click(files), &mut state, area);
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(state.selected, Some(1));
    }

    #[test]
    fn mouse_click_can_focus_diff_pane_when_file_list_is_empty() {
        let mut state = state(1, vec![]);
        let area = Rect::new(0, 0, 100, 30);
        let (_, diff) = ui::pane_areas(area);

        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: diff.x + 1,
                row: diff.y + 1,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            &mut state,
            area,
        );

        assert_eq!(state.focus, Focus::Diff);
    }

    #[test]
    fn mouse_wheel_scrolls_only_over_diff_pane() {
        let mut state = state(1, vec![file("a", 1)]);
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
