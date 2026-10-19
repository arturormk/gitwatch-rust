use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

use gitwatch_rust::document::{DocumentContent, MarkdownDocument};

use crate::highlight::SyntaxHighlighter;

const MAX_RENDERED_LINES: usize = 100_000;
const MAX_RENDERED_LINE_BYTES: usize = 16_384;

#[derive(Debug, Default)]
pub(crate) struct MarkdownCache {
    key: Option<(PathBuf, u64)>,
    document: RenderedMarkdown,
}

impl MarkdownCache {
    pub(crate) fn lines(
        &mut self,
        document: &MarkdownDocument,
        highlighter: &SyntaxHighlighter,
    ) -> &RenderedMarkdown {
        let key = document_key(document);
        if self.key.as_ref() != Some(&key) {
            self.document = render_document(document, highlighter);
            self.key = Some(key);
        }
        &self.document
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RenderedMarkdown {
    lines: Vec<RenderedLine>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RenderedLine {
    line: Line<'static>,
    wrap: bool,
}

impl RenderedMarkdown {
    pub(crate) fn layout(&self, width: usize) -> Vec<Line<'static>> {
        let width = width.max(1);
        self.lines
            .iter()
            .flat_map(|rendered| {
                if rendered.wrap {
                    wrap_line(&rendered.line, width)
                } else {
                    vec![rendered.line.clone()]
                }
            })
            .collect()
    }
}

pub(crate) fn render_document(
    document: &MarkdownDocument,
    highlighter: &SyntaxHighlighter,
) -> RenderedMarkdown {
    match &document.content {
        DocumentContent::Text(markdown) => render_markdown(markdown, highlighter),
        DocumentContent::TooLarge { size, limit } => RenderedMarkdown::single(Line::from(format!(
            " Document is too large to display ({size} bytes; limit {limit} bytes)."
        ))),
        DocumentContent::Unavailable(message) => RenderedMarkdown::single(Line::styled(
            format!(" Document unavailable: {message}"),
            Style::default().fg(Color::Red),
        )),
    }
}

fn document_key(document: &MarkdownDocument) -> (PathBuf, u64) {
    let mut hasher = DefaultHasher::new();
    document.path.hash(&mut hasher);
    match &document.content {
        DocumentContent::Text(text) => text.hash(&mut hasher),
        DocumentContent::TooLarge { size, limit } => {
            size.hash(&mut hasher);
            limit.hash(&mut hasher);
        }
        DocumentContent::Unavailable(message) => message.hash(&mut hasher),
    }
    (document.path.clone(), hasher.finish())
}

fn render_markdown(markdown: &str, highlighter: &SyntaxHighlighter) -> RenderedMarkdown {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;
    let mut renderer = MarkdownRenderer::new(highlighter);
    for event in Parser::new_ext(markdown, options) {
        renderer.event(event);
        if renderer.lines.len() >= MAX_RENDERED_LINES {
            renderer.flush();
            renderer.push_line(
                Line::styled(
                    "… document rendering stopped at the line limit …",
                    Style::default().fg(Color::Yellow),
                ),
                true,
            );
            break;
        }
    }
    renderer.finish()
}

struct MarkdownRenderer<'a> {
    highlighter: &'a SyntaxHighlighter,
    lines: Vec<RenderedLine>,
    spans: Vec<Span<'static>>,
    style: Style,
    inline_styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    quote_depth: usize,
    code_path: Option<PathBuf>,
    links: Vec<String>,
}

impl<'a> MarkdownRenderer<'a> {
    fn new(highlighter: &'a SyntaxHighlighter) -> Self {
        Self {
            highlighter,
            lines: Vec::new(),
            spans: Vec::new(),
            style: Style::default(),
            inline_styles: Vec::new(),
            lists: Vec::new(),
            quote_depth: 0,
            code_path: None,
            links: Vec::new(),
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => self.spans.push(Span::styled(
                code.into_string(),
                self.style.fg(Color::Yellow).bg(Color::DarkGray),
            )),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.push_line(
                    Line::styled(
                        "────────────────────────────────────────",
                        Style::default().fg(Color::DarkGray),
                    ),
                    true,
                );
            }
            Event::TaskListMarker(checked) => {
                self.push_text(if checked { "[x] " } else { "[ ] " });
            }
            Event::Html(_) | Event::InlineHtml(_) => {
                self.spans
                    .push(Span::styled("[HTML]", Style::default().fg(Color::DarkGray)));
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.prefix_block(),
            Tag::Heading { level, .. } => {
                self.flush();
                self.prefix_block();
                self.inline_styles.push(self.style);
                self.style = heading_style(level);
                self.push_text(&format!("{} ", heading_marker(level)));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.code_path = Some(code_path(&kind));
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                self.prefix_block();
                let prefix = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let prefix = format!("{number}. ");
                        *number = number.saturating_add(1);
                        prefix
                    }
                    _ => "• ".to_owned(),
                };
                self.push_text(&prefix);
            }
            Tag::Emphasis => self.push_style(self.style.add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(self.style.add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_style(self.style.add_modifier(Modifier::CROSSED_OUT));
            }
            Tag::Link { dest_url, .. } => {
                self.links.push(dest_url.into_string());
                self.push_style(
                    self.style
                        .fg(Color::Blue)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Tag::Image { dest_url, .. } => {
                self.links.push(dest_url.into_string());
                self.push_text("[image: ");
                self.push_style(self.style.fg(Color::Magenta));
            }
            Tag::Table(_) => self.flush(),
            Tag::TableHead | Tag::TableRow => self.flush(),
            Tag::TableCell if !self.spans.is_empty() => self.push_text(" │ "),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.finish_block(),
            TagEnd::Heading(_) => {
                self.pop_style();
                self.finish_block();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.blank_line();
            }
            TagEnd::CodeBlock => {
                self.flush();
                self.code_path = None;
                self.blank_line();
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank_line();
                }
            }
            TagEnd::Item => self.flush(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => {
                self.pop_style();
                if let Some(destination) = self.links.pop() {
                    self.spans.push(Span::styled(
                        format!(" ({destination})"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
            TagEnd::Image => {
                self.pop_style();
                let destination = self.links.pop().unwrap_or_default();
                self.push_text(&format!("] ({destination})"));
            }
            TagEnd::Table => {
                self.flush();
                self.blank_line();
            }
            TagEnd::TableHead | TagEnd::TableRow => self.flush(),
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if let Some(path) = self.code_path.clone() {
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    self.flush();
                }
                if !line.is_empty() {
                    self.prefix_block();
                    let spans = self.highlighter.line(&path, line);
                    self.spans.extend(spans.into_iter().map(|span| {
                        Span::styled(span.content.into_owned(), span.style.bg(Color::DarkGray))
                    }));
                }
            }
            return;
        }
        for (index, line) in text.split('\n').enumerate() {
            if index > 0 {
                self.flush();
                self.prefix_block();
            }
            self.push_text(line);
        }
    }

    fn prefix_block(&mut self) {
        if self.spans.is_empty() {
            let indent = "  ".repeat(self.lists.len().saturating_sub(1));
            let quote = "│ ".repeat(self.quote_depth);
            if !indent.is_empty() || !quote.is_empty() {
                self.spans.push(Span::styled(
                    format!("{indent}{quote}"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
    }

    fn push_text(&mut self, text: &str) {
        if !text.is_empty() {
            self.spans.push(Span::styled(truncate(text), self.style));
        }
    }

    fn push_style(&mut self, style: Style) {
        self.inline_styles.push(self.style);
        self.style = style;
    }

    fn pop_style(&mut self) {
        self.style = self.inline_styles.pop().unwrap_or_default();
    }

    fn finish_block(&mut self) {
        self.flush();
        self.blank_line();
    }

    fn blank_line(&mut self) {
        if self
            .lines
            .last()
            .is_none_or(|rendered| !rendered.line.spans.is_empty())
        {
            self.push_line(Line::default(), true);
        }
    }

    fn flush(&mut self) {
        if !self.spans.is_empty() {
            let wrap = self.code_path.is_none();
            let line = Line::from(std::mem::take(&mut self.spans));
            self.push_line(line, wrap);
        }
    }

    fn push_line(&mut self, line: Line<'static>, wrap: bool) {
        self.lines.push(RenderedLine { line, wrap });
    }

    fn finish(mut self) -> RenderedMarkdown {
        self.flush();
        while self
            .lines
            .last()
            .is_some_and(|rendered| rendered.line.spans.is_empty())
        {
            self.lines.pop();
        }
        if self.lines.is_empty() {
            self.push_line(Line::from(" Empty document."), true);
        }
        RenderedMarkdown { lines: self.lines }
    }
}

impl RenderedMarkdown {
    fn single(line: Line<'static>) -> Self {
        Self {
            lines: vec![RenderedLine { line, wrap: true }],
        }
    }
}

fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    if line.width() <= width || line.spans.is_empty() {
        return vec![line.clone()];
    }

    let text = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        while start < text.len()
            && text[start..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
        {
            start += text[start..].chars().next().unwrap().len_utf8();
        }
        if start >= text.len() {
            break;
        }
        let mut used = 0;
        let mut end = start;
        let mut last_break = None;
        for (offset, character) in text[start..].char_indices() {
            let character_width = UnicodeWidthChar::width(character).unwrap_or_default();
            if used + character_width > width {
                break;
            }
            used += character_width;
            end = start + offset + character.len_utf8();
            if character.is_whitespace() {
                last_break = Some(end);
            }
        }
        if end == start {
            end = start + text[start..].chars().next().unwrap().len_utf8();
        } else if end < text.len() {
            end = last_break
                .filter(|boundary| *boundary > start)
                .unwrap_or(end);
        }
        chunks.push(styled_slice(line, start, end));
        start = end;
    }
    if chunks.is_empty() {
        chunks.push(Line::default());
    }
    chunks
}

fn styled_slice(line: &Line<'static>, start: usize, end: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let mut offset = 0;
    for span in &line.spans {
        let span_start = offset;
        let span_end = offset + span.content.len();
        let overlap_start = start.max(span_start);
        let overlap_end = end.min(span_end);
        if overlap_start < overlap_end {
            spans.push(Span::styled(
                span.content[overlap_start - span_start..overlap_end - span_start].to_owned(),
                span.style,
            ));
        }
        offset = span_end;
    }
    Line::from(spans).style(line.style)
}

fn heading_marker(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "#",
        HeadingLevel::H2 => "##",
        HeadingLevel::H3 => "###",
        HeadingLevel::H4 => "####",
        HeadingLevel::H5 => "#####",
        HeadingLevel::H6 => "######",
    }
}

fn heading_style(level: HeadingLevel) -> Style {
    let color = match level {
        HeadingLevel::H1 => Color::Cyan,
        HeadingLevel::H2 => Color::Blue,
        _ => Color::Magenta,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn code_path(kind: &CodeBlockKind<'_>) -> PathBuf {
    let language = match kind {
        CodeBlockKind::Indented => return PathBuf::from("code.txt"),
        CodeBlockKind::Fenced(language) => language.as_ref(),
    };
    let extension = match language.split_whitespace().next().unwrap_or_default() {
        "rust" | "rs" => "rs",
        "javascript" | "js" => "js",
        "typescript" | "ts" => "ts",
        "python" | "py" => "py",
        "bash" | "shell" | "sh" => "sh",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        other if !other.is_empty() => other,
        _ => "txt",
    };
    Path::new("code").with_extension(extension)
}

fn truncate(text: &str) -> String {
    if text.len() <= MAX_RENDERED_LINE_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_RENDERED_LINE_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(markdown: &str) -> Vec<Line<'static>> {
        render_markdown(markdown, &SyntaxHighlighter::new()).layout(usize::MAX)
    }

    #[test]
    fn renders_headings_lists_links_quotes_and_code() {
        let lines = rendered(
            "# Title\n\n- **bold** [link](https://example.com)\n\n> quote\n\n```rust\npub fn main() {}\n```",
        );
        let text = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("# Title"));
        assert!(text.contains("• bold link (https://example.com)"));
        assert!(text.contains("│ quote"));
        assert!(text.contains("pub fn main() {}"));
    }

    #[test]
    fn empty_markdown_has_an_explicit_message() {
        let lines = rendered("");
        assert_eq!(lines[0].spans[0].content, " Empty document.");
    }

    #[test]
    fn cache_invalidates_when_open_document_content_changes() {
        let highlighter = SyntaxHighlighter::new();
        let mut cache = MarkdownCache::default();
        let mut document = MarkdownDocument {
            path: PathBuf::from("README.md"),
            content: DocumentContent::Text("# Before".into()),
        };
        let before = cache.lines(&document, &highlighter).layout(80)[0].clone();
        document.content = DocumentContent::Text("# After".into());
        let after = cache.lines(&document, &highlighter).layout(80)[0].clone();
        assert_ne!(before, after);
    }

    #[test]
    fn wraps_prose_but_preserves_long_code_lines() {
        let rendered = render_markdown(
            "This paragraph has several words.\n\n```text\n0123456789abcdef\n```",
            &SyntaxHighlighter::new(),
        );
        let lines = rendered.layout(8);
        assert!(lines.len() > 2);
        assert!(lines.iter().any(|line| line.width() == 16));
    }

    #[test]
    fn renders_tables_tasks_images_and_html_placeholders() {
        let lines = rendered(
            "| A | B |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n\n![diagram](image.png)\n\n<div>raw</div>",
        );
        let text = lines
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("A │ B"));
        assert!(text.contains("[x] done"));
        assert!(text.contains("[image: diagram] (image.png)"));
        assert!(text.contains("[HTML]"));
    }
}
