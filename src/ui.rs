use std::path::Path;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use gitwatch_rust::model::{ChangeStatus, DiffRow, FileChange, FileKind};

use crate::{
    app::{AppState, Focus},
    highlight::SyntaxHighlighter,
};

pub(crate) fn render(frame: &mut Frame<'_>, state: &mut AppState, highlighter: &SyntaxHighlighter) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());
    render_header(frame, areas[0], state);

    if state.snapshot.files.is_empty() {
        let message = if let Some(error) = &state.error {
            format!("Refresh failed: {error}\n\nPress r to retry or q to quit.")
        } else {
            "Working tree clean — waiting for changes...".to_owned()
        };
        frame.render_widget(
            Paragraph::new(message)
                .centered()
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Unified diff "),
                ),
            areas[1],
        );
    } else {
        let left_width = if areas[1].width < 48 {
            (areas[1].width / 3).max(12)
        } else {
            (areas[1].width * 28 / 100).clamp(20, 42)
        };
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(left_width), Constraint::Min(1)])
            .split(areas[1]);
        render_files(frame, body[0], state);
        render_diff(frame, body[1], state, highlighter);
    }
    render_footer(frame, areas[2], state);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let status = if state.refreshing {
        ("refreshing", Color::Yellow)
    } else if state.error.is_some() {
        ("error", Color::Red)
    } else {
        ("live", Color::Green)
    };
    let mut spans = vec![
        Span::styled(" gitwatch ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            "{}  unified  ",
            display_path(&state.snapshot.repository_root)
        )),
        Span::styled(status.0, Style::default().fg(status.1)),
    ];
    if !state.snapshot.warnings.is_empty() {
        spans.push(Span::styled(
            format!("  {} warning(s)", state.snapshot.warnings.len()),
            Style::default().fg(Color::Yellow),
        ));
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
    let (lines, title) = {
        let Some(file) = state.selected_file() else {
            return;
        };
        (
            unified_lines(file, highlighter),
            format!(" Unified — {} ", display_path(file.path())),
        )
    };
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

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let text = if let Some(error) = &state.error {
        format!(" error: {error}  •  r retry  q quit")
    } else if area.width < 72 {
        " Tab focus  ↑↓ move  n/p hunk  r refresh  q quit".to_owned()
    } else {
        " Tab focus  ↑↓/j/k move  ←→/h/l scroll  n/p hunk  r refresh  q quit".to_owned()
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().add_modifier(Modifier::DIM)),
        area,
    );
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
    use super::*;

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
}
