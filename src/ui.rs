use std::{collections::HashSet, path::Path};

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use gitwatch_rust::model::{ChangeStatus, DiffRow, FileChange, FileKind};

use crate::{
    app::{AppState, DiffPresentation, Focus, ViewMode},
    highlight::SyntaxHighlighter,
};

pub(crate) fn render(frame: &mut Frame<'_>, state: &mut AppState, highlighter: &SyntaxHighlighter) {
    let areas = screen_areas(frame.area());
    render_header(frame, areas.0, state);

    if state.active_file_count() == 0 {
        let (message, title) = match state.view_mode {
            ViewMode::Diff => (
                if let Some(error) = &state.error {
                    format!("Refresh failed: {error}\n\nPress r to retry or q to quit.")
                } else {
                    "Working tree clean — waiting for changes...".to_owned()
                },
                match state.diff_presentation {
                    DiffPresentation::Unified => " Unified diff ",
                    DiffPresentation::Before => " Before version ",
                    DiffPresentation::After => " After version ",
                },
            ),
            ViewMode::Documentation => (
                if let Some(error) = &state.error {
                    format!("Documentation refresh failed: {error}\n\nPress r to retry.")
                } else {
                    "No *.md files found in this repository.".to_owned()
                },
                " Documentation ",
            ),
        };
        frame.render_widget(
            Paragraph::new(message)
                .centered()
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title(title)),
            areas.1,
        );
    } else {
        let (files, diff) = body_panes(areas.1);
        match state.view_mode {
            ViewMode::Diff => {
                render_files(frame, files, state);
                render_diff(frame, diff, state, highlighter);
            }
            ViewMode::Documentation => {
                render_document_list(frame, files, state);
                render_document_content(frame, diff, state, highlighter);
            }
        }
    }
    render_footer(frame, areas.2, state);
}

fn screen_areas(area: Rect) -> (Rect, Rect, Rect) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
    (areas[0], areas[1], areas[2])
}

fn body_panes(body: Rect) -> (Rect, Rect) {
    let left_width = if body.width < 48 {
        (body.width / 3).max(12)
    } else {
        (body.width * 28 / 100).clamp(20, 42)
    };
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(left_width), Constraint::Min(1)])
        .split(body);
    (panes[0], panes[1])
}

pub(crate) fn pane_areas(area: Rect) -> (Rect, Rect) {
    body_panes(screen_areas(area).1)
}

fn render_header(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let status = if state.refreshing {
        ("refreshing", Color::Yellow)
    } else if state.pending {
        ("pending", Color::Yellow)
    } else if state.error.is_some() {
        ("error", Color::Red)
    } else {
        ("live", Color::Green)
    };
    let mode = match state.view_mode {
        ViewMode::Diff => match state.diff_presentation {
            DiffPresentation::Unified => "unified",
            DiffPresentation::Before => "before",
            DiffPresentation::After => "after",
        },
        ViewMode::Documentation => "documentation",
    };
    let mut spans = vec![
        Span::styled(" gitwatch ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            "{}  {mode}  ",
            display_path(&state.snapshot.repository_root),
        )),
        Span::styled(status.0, Style::default().fg(status.1)),
    ];
    if !state.snapshot.warnings.is_empty() {
        spans.push(Span::styled(
            format!("  {} warning(s)", state.snapshot.warnings.len()),
            Style::default().fg(Color::Yellow),
        ));
    }
    if state.view_mode == ViewMode::Documentation {
        let warning_count = state.documentation.warnings.len();
        if warning_count > 0 || state.documentation.incomplete {
            spans.push(Span::styled(
                format!(
                    "  {} documentation warning(s)",
                    warning_count + usize::from(state.documentation.incomplete)
                ),
                Style::default().fg(Color::Yellow),
            ));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_files(frame: &mut Frame<'_>, area: Rect, state: &mut AppState) {
    let items: Vec<ListItem<'_>> = state
        .snapshot
        .files
        .iter()
        .map(|file| {
            let index = if file.staged { 'S' } else { ' ' };
            let worktree = if file.unstaged { 'W' } else { ' ' };
            ListItem::new(format!(
                "{} {index}{worktree} {}",
                status_code(file.status),
                display_path(file.path())
            ))
        })
        .collect();
    let border = if state.focus == Focus::Files {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let title = state
        .selected_file()
        .map(|file| format!(" Files — {} ", display_path(file.path())))
        .unwrap_or_else(|| " Files ".to_owned());
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border))
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    let mut list_state = ListState::default()
        .with_selected(state.selected)
        .with_offset(state.file_offset);
    frame.render_stateful_widget(list, area, &mut list_state);
    state.file_offset = list_state.offset();
}

fn render_diff(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &mut AppState,
    highlighter: &SyntaxHighlighter,
) {
    let (mut lines, title) = {
        let Some(file) = state.selected_file() else {
            return;
        };
        match state.diff_presentation {
            DiffPresentation::Unified => (
                unified_lines(file, highlighter),
                format!(" Unified — {} ", display_path(file.path())),
            ),
            DiffPresentation::Before => (
                version_lines(file, DiffPresentation::Before, highlighter),
                format!(
                    " Before — {} ",
                    display_path(version_path(file, DiffPresentation::Before))
                ),
            ),
            DiffPresentation::After => (
                version_lines(file, DiffPresentation::After, highlighter),
                format!(
                    " After — {} ",
                    display_path(version_path(file, DiffPresentation::After))
                ),
            ),
        }
    };
    state.update_search_content(lines.iter().map(line_text).collect());
    highlight_search(&mut lines, state.search_query());
    let visible_height = area.height.saturating_sub(2) as usize;
    state.diff_y = state
        .diff_y
        .min(lines.len().saturating_sub(visible_height.max(1)));
    let border = if state.focus == Focus::Diff {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border))
                .title(title),
        )
        .scroll((
            state.diff_y.min(u16::MAX as usize) as u16,
            state.diff_x.min(u16::MAX as usize) as u16,
        ));
    frame.render_widget(paragraph, area);
}

fn render_document_list(frame: &mut Frame<'_>, area: Rect, state: &mut AppState) {
    let items = state
        .documentation
        .documents
        .iter()
        .map(|document| ListItem::new(display_path(&document.path)))
        .collect::<Vec<_>>();
    let border = if state.focus == Focus::Files {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let title = state
        .selected_document()
        .map(|document| format!(" Markdown — {} ", display_path(&document.path)))
        .unwrap_or_else(|| " Markdown ".to_owned());
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border))
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    let mut list_state = ListState::default()
        .with_selected(state.document_selected)
        .with_offset(state.document_offset);
    frame.render_stateful_widget(list, area, &mut list_state);
    state.document_offset = list_state.offset();
}

fn render_document_content(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &mut AppState,
    highlighter: &SyntaxHighlighter,
) {
    let Some(document) = state.selected_document().cloned() else {
        return;
    };
    let title = format!(" Rendered — {} ", display_path(&document.path));
    let content_width = usize::from(area.width.saturating_sub(2)).max(1);
    let mut lines = state
        .markdown_cache
        .lines(&document, highlighter)
        .layout(content_width);
    state.update_search_content(lines.iter().map(line_text).collect());
    highlight_search(&mut lines, state.search_query());
    let rendered_height = lines.len();
    let border = if state.focus == Focus::Diff {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border))
            .title(title),
    );
    let visible_height = area.height.saturating_sub(2) as usize;
    state.document_y = state
        .document_y
        .min(rendered_height.saturating_sub(visible_height.max(1)));
    let paragraph = paragraph.scroll((
        state.document_y.min(u16::MAX as usize) as u16,
        state.document_x.min(u16::MAX as usize) as u16,
    ));
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let text = if let Some(prompt) = state.search_prompt() {
        format!("/{prompt}")
    } else if let Some(error) = &state.error {
        format!(" error: {error}  •  r retry  q quit")
    } else if let Some(status) = state.search_status() {
        format!(" {status}  n/N next/previous  / new search  Esc clear")
    } else if state.view_mode == ViewMode::Documentation && area.width < 72 {
        " d diffs  / search  Tab focus  ↑↓ move  q quit".to_owned()
    } else if state.view_mode == ViewMode::Documentation {
        " d diffs  / search  Tab focus  ↑↓/j/k move  ←→/h/l scroll  t shell  q quit".to_owned()
    } else if area.width < 72 {
        " v/V view  d docs  / search  ↑↓ move  n/p hunk  q quit".to_owned()
    } else {
        " v/V view  d docs  / search  Tab focus  ↑↓/j/k move  ←→/h/l scroll  n/p hunk  t shell  q quit".to_owned()
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().add_modifier(Modifier::DIM)),
        area,
    );
    if let Some(prompt) = state.search_prompt() {
        let cursor = 1usize
            .saturating_add(unicode_width::UnicodeWidthStr::width(prompt))
            .min(usize::from(area.width.saturating_sub(1)));
        frame.set_cursor_position((area.x.saturating_add(cursor as u16), area.y));
    }
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn highlight_search(lines: &mut [Line<'static>], query: &str) {
    if query.is_empty() {
        return;
    }
    let highlight = Style::default().fg(Color::Black).bg(Color::Yellow);
    for line in lines {
        let text = line_text(line);
        let matches = text
            .match_indices(query)
            .map(|(start, found)| start..start + found.len())
            .collect::<Vec<_>>();
        if matches.is_empty() {
            continue;
        }
        let mut offset = 0;
        let mut spans = Vec::new();
        for span in &line.spans {
            let content = span.content.as_ref();
            let span_start = offset;
            let span_end = offset + content.len();
            let mut cursor = 0;
            for found in &matches {
                let start = found.start.max(span_start).min(span_end) - span_start;
                let end = found.end.max(span_start).min(span_end) - span_start;
                if start > cursor {
                    spans.push(Span::styled(content[cursor..start].to_owned(), span.style));
                }
                if end > start {
                    spans.push(Span::styled(
                        content[start..end].to_owned(),
                        span.style.patch(highlight),
                    ));
                    cursor = end;
                }
            }
            if cursor < content.len() {
                spans.push(Span::styled(content[cursor..].to_owned(), span.style));
            }
            offset = span_end;
        }
        line.spans = spans;
    }
}

fn unified_lines(file: &FileChange, highlighter: &SyntaxHighlighter) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if file.old_path != file.new_path || file.mode_changed {
        let mut metadata = Vec::new();
        if file.old_path != file.new_path {
            metadata.push(format!(
                "renamed {} → {}",
                file.old_path
                    .as_deref()
                    .map(display_path)
                    .unwrap_or_else(|| "/dev/null".into()),
                file.new_path
                    .as_deref()
                    .map(display_path)
                    .unwrap_or_else(|| "/dev/null".into())
            ));
        }
        if file.mode_changed {
            metadata.push(format!(
                "mode {:06o} → {:06o}",
                file.old_mode.unwrap_or_default(),
                file.new_mode.unwrap_or_default()
            ));
        }
        lines.push(Line::styled(
            format!(" {}", metadata.join("; ")),
            Style::default().fg(Color::Yellow),
        ));
    }

    match &file.kind {
        FileKind::Text | FileKind::Symlink => {
            for hunk in &file.diff.hunks {
                lines.push(Line::styled(
                    format!(
                        "@@ -{},{} +{},{} @@",
                        hunk.old_start, hunk.old_len, hunk.new_start, hunk.new_len
                    ),
                    Style::default().fg(Color::Cyan),
                ));
                for row in &hunk.rows {
                    match row {
                        DiffRow::Equal {
                            old_line,
                            new_line,
                            content,
                        } => lines.push(source_line(
                            ' ',
                            Some(*old_line),
                            Some(*new_line),
                            content,
                            file.path(),
                            highlighter,
                            Style::default(),
                        )),
                        DiffRow::Delete { old_line, content } => lines.push(source_line(
                            '-',
                            Some(*old_line),
                            None,
                            content,
                            file.path(),
                            highlighter,
                            Style::default().bg(Color::Rgb(64, 20, 24)),
                        )),
                        DiffRow::Insert { new_line, content } => lines.push(source_line(
                            '+',
                            None,
                            Some(*new_line),
                            content,
                            file.path(),
                            highlighter,
                            Style::default().bg(Color::Rgb(16, 58, 36)),
                        )),
                        DiffRow::Replace {
                            old_line,
                            old_content,
                            new_line,
                            new_content,
                        } => {
                            lines.push(source_line(
                                '-',
                                Some(*old_line),
                                None,
                                old_content,
                                file.path(),
                                highlighter,
                                Style::default().bg(Color::Rgb(64, 20, 24)),
                            ));
                            lines.push(source_line(
                                '+',
                                None,
                                Some(*new_line),
                                new_content,
                                file.path(),
                                highlighter,
                                Style::default().bg(Color::Rgb(16, 58, 36)),
                            ));
                        }
                    }
                }
            }
            if lines.is_empty() {
                lines.push(Line::from(" No textual changes (metadata only)."));
            }
        }
        FileKind::Binary { old_size, new_size } => lines.push(Line::from(format!(
            " Binary file changed ({old_size} → {new_size} bytes)."
        ))),
        FileKind::TooLarge {
            limit,
            old_size,
            new_size,
        } => lines.push(Line::from(format!(
            " File exceeds the {limit} byte display limit ({old_size} → {new_size} bytes)."
        ))),
        FileKind::Submodule => lines.push(Line::from(" Submodule changed.")),
        FileKind::Unavailable { message } => {
            lines.push(Line::styled(format!(" Unavailable: {message}"), Color::Red));
        }
    }
    lines
}

fn version_lines(
    file: &FileChange,
    presentation: DiffPresentation,
    highlighter: &SyntaxHighlighter,
) -> Vec<Line<'static>> {
    let content = match presentation {
        DiffPresentation::Before => file.old_content.as_ref(),
        DiffPresentation::After => file.new_content.as_ref(),
        DiffPresentation::Unified => unreachable!("unified rendering uses unified_lines"),
    };
    let Some(content) = content else {
        let label = match presentation {
            DiffPresentation::Before => "before",
            DiffPresentation::After => "after",
            DiffPresentation::Unified => unreachable!(),
        };
        return vec![Line::styled(
            format!(" No {label} text version is available."),
            Style::default().fg(Color::Yellow),
        )];
    };
    if content.text.is_empty() {
        return vec![Line::styled(
            " (empty file)",
            Style::default().add_modifier(Modifier::DIM),
        )];
    }
    let changed_lines = version_changed_lines(file, presentation);
    content
        .text
        .lines()
        .enumerate()
        .map(|(index, text)| {
            let number = index + 1;
            version_line(
                number,
                text,
                version_path(file, presentation),
                highlighter,
                changed_lines
                    .contains(&number)
                    .then(|| version_change_style(presentation)),
            )
        })
        .collect()
}

fn version_changed_lines(file: &FileChange, presentation: DiffPresentation) -> HashSet<usize> {
    file.diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter_map(|row| match (presentation, row) {
            (
                DiffPresentation::Before,
                DiffRow::Delete { old_line, .. } | DiffRow::Replace { old_line, .. },
            ) => Some(*old_line),
            (
                DiffPresentation::After,
                DiffRow::Insert { new_line, .. } | DiffRow::Replace { new_line, .. },
            ) => Some(*new_line),
            _ => None,
        })
        .collect()
}

fn version_change_style(presentation: DiffPresentation) -> Style {
    match presentation {
        DiffPresentation::Before => Style::default().bg(Color::Rgb(64, 20, 24)),
        DiffPresentation::After => Style::default().bg(Color::Rgb(16, 58, 36)),
        DiffPresentation::Unified => Style::default(),
    }
}

fn version_path(file: &FileChange, presentation: DiffPresentation) -> &Path {
    match presentation {
        DiffPresentation::Before => file.old_path.as_deref(),
        DiffPresentation::After => file.new_path.as_deref(),
        DiffPresentation::Unified => None,
    }
    .unwrap_or(&file.key)
}

fn version_line(
    number: usize,
    content: &str,
    path: &Path,
    highlighter: &SyntaxHighlighter,
    semantic_style: Option<Style>,
) -> Line<'static> {
    let semantic_style = semantic_style.unwrap_or_default();
    let mut spans = vec![Span::styled(
        format!("{number:>6}  "),
        semantic_style.add_modifier(Modifier::DIM),
    )];
    spans.extend(highlighter.line(path, content));
    Line::from(spans).style(semantic_style)
}

#[allow(clippy::too_many_arguments)]
fn source_line(
    prefix: char,
    old_line: Option<usize>,
    new_line: Option<usize>,
    content: &str,
    path: &Path,
    highlighter: &SyntaxHighlighter,
    semantic_style: Style,
) -> Line<'static> {
    let old = old_line.map(|line| line.to_string()).unwrap_or_default();
    let new = new_line.map(|line| line.to_string()).unwrap_or_default();
    let mut spans = vec![Span::styled(
        format!("{old:>5} {new:>5} {prefix} "),
        semantic_style.add_modifier(Modifier::DIM),
    )];
    spans.extend(highlighter.line(path, content));
    Line::from(spans).style(semantic_style)
}

fn status_code(status: ChangeStatus) -> char {
    match status {
        ChangeStatus::Modified => 'M',
        ChangeStatus::Added => 'A',
        ChangeStatus::Deleted => 'D',
        ChangeStatus::Renamed => 'R',
        ChangeStatus::TypeChanged => 'T',
        ChangeStatus::Untracked => '?',
        ChangeStatus::Conflicted => 'U',
    }
}

#[cfg(unix)]
fn display_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    let mut output = String::new();
    let mut rest = path.as_os_str().as_bytes();
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                output.extend(valid.chars().flat_map(char::escape_default));
                break;
            }
            Err(error) => {
                let valid = &rest[..error.valid_up_to()];
                output.extend(
                    String::from_utf8_lossy(valid)
                        .chars()
                        .flat_map(char::escape_default),
                );
                let invalid = error
                    .error_len()
                    .unwrap_or(rest.len() - error.valid_up_to());
                for byte in &rest[error.valid_up_to()..error.valid_up_to() + invalid] {
                    output.push_str(&format!("\\x{byte:02x}"));
                }
                rest = &rest[error.valid_up_to() + invalid..];
            }
        }
    }
    output
}

#[cfg(not(unix))]
fn display_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .flat_map(char::escape_default)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use gitwatch_rust::model::{DiffHunk, FileDiff, TextContent};

    use super::*;

    fn text_file() -> FileChange {
        FileChange {
            key: PathBuf::from("src/main.rs"),
            old_path: Some(PathBuf::from("src/main.rs")),
            new_path: Some(PathBuf::from("src/main.rs")),
            status: ChangeStatus::Modified,
            staged: false,
            unstaged: true,
            kind: FileKind::Text,
            old_content: Some(TextContent {
                text: "fn before() {}\nlet removed = true;\n".into(),
                has_final_newline: true,
            }),
            new_content: Some(TextContent {
                text: "fn after() {}\nlet value = 1;\n".into(),
                has_final_newline: true,
            }),
            old_mode: Some(0o100644),
            new_mode: Some(0o100644),
            mode_changed: false,
            diff: FileDiff {
                hunks: vec![DiffHunk {
                    old_start: 1,
                    old_len: 2,
                    new_start: 1,
                    new_len: 2,
                    rows: vec![
                        DiffRow::Replace {
                            old_line: 1,
                            old_content: "fn before() {}".into(),
                            new_line: 1,
                            new_content: "fn after() {}".into(),
                        },
                        DiffRow::Delete {
                            old_line: 2,
                            content: "let removed = true;".into(),
                        },
                        DiffRow::Insert {
                            new_line: 2,
                            content: "let value = 1;".into(),
                        },
                    ],
                }],
            },
        }
    }

    #[test]
    fn status_symbols_cover_every_change_kind() {
        assert_eq!(status_code(ChangeStatus::Modified), 'M');
        assert_eq!(status_code(ChangeStatus::Untracked), '?');
        assert_eq!(status_code(ChangeStatus::Conflicted), 'U');
    }

    #[test]
    fn control_characters_are_escaped_for_terminal_output() {
        assert_eq!(display_path(Path::new("a\tb\nc")), "a\\tb\\nc");
    }

    #[test]
    fn search_highlighting_can_cross_styled_spans() {
        let mut lines = vec![Line::from(vec![
            Span::styled("foo", Style::default().fg(Color::Red)),
            Span::styled("bar", Style::default().fg(Color::Green)),
        ])];

        highlight_search(&mut lines, "oob");

        assert_eq!(line_text(&lines[0]), "foobar");
        let highlighted = lines[0]
            .spans
            .iter()
            .filter(|span| span.style.bg == Some(Color::Yellow))
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(highlighted, "oob");
    }

    #[test]
    fn version_views_render_complete_numbered_file_contents() {
        let file = text_file();
        let highlighter = SyntaxHighlighter::new();

        let before = version_lines(&file, DiffPresentation::Before, &highlighter);
        let after = version_lines(&file, DiffPresentation::After, &highlighter);

        assert_eq!(before.len(), 2);
        assert!(line_text(&before[0]).contains("1  fn before() {}"));
        assert!(
            before
                .iter()
                .all(|line| line.style.bg == Some(Color::Rgb(64, 20, 24)))
        );
        assert_eq!(after.len(), 2);
        assert!(line_text(&after[1]).contains("2  let value = 1;"));
        assert!(
            after
                .iter()
                .all(|line| line.style.bg == Some(Color::Rgb(16, 58, 36)))
        );
    }
}
