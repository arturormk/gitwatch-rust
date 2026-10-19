use std::path::Path;

use ratatui::{
    style::{Color, Style},
    text::Span,
};

pub(crate) struct SyntaxHighlighter;

impl SyntaxHighlighter {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn line(&self, path: &Path, content: &str) -> Vec<Span<'static>> {
        let Some(language) = language_for_path(path) else {
            return vec![Span::raw(content.to_owned())];
        };
        colorize(content, language)
    }
}

#[derive(Clone, Copy)]
enum Language {
    CStyle,
    HashComment,
    Rust,
}

fn language_for_path(path: &Path) -> Option<Language> {
    match path.extension().and_then(|value| value.to_str())? {
        "rs" => Some(Language::Rust),
        "c" | "cc" | "cpp" | "css" | "go" | "h" | "hpp" | "java" | "js" | "jsx" | "kt" | "kts"
        | "php" | "swift" | "ts" | "tsx" => Some(Language::CStyle),
        "bash" | "fish" | "py" | "rb" | "sh" | "toml" | "yaml" | "yml" => {
            Some(Language::HashComment)
        }
        _ => None,
    }
}

fn colorize(content: &str, language: Language) -> Vec<Span<'static>> {
    let comment_start = match language {
        Language::CStyle | Language::Rust => content.find("//"),
        Language::HashComment => content.find('#'),
    };
    let (code, comment) = comment_start
        .map(|index| content.split_at(index))
        .unwrap_or((content, ""));
    let mut spans = colorize_code(code, language);
    if !comment.is_empty() {
        spans.push(Span::styled(
            comment.to_owned(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans
}

fn colorize_code(code: &str, language: Language) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = code.char_indices().collect();
    let mut index = 0;
    while index < chars.len() {
        let (byte, character) = chars[index];
        if matches!(character, '"' | '\'') {
            push_plain(&mut spans, &code[start..byte]);
            let quote = character;
            let mut end = code.len();
            index += 1;
            while index < chars.len() {
                let (next_byte, next) = chars[index];
                if next == quote && !code[..next_byte].ends_with('\\') {
                    end = next_byte + next.len_utf8();
                    index += 1;
                    break;
                }
                index += 1;
            }
            spans.push(Span::styled(
                code[byte..end].to_owned(),
                Style::default().fg(Color::Green),
            ));
            start = end;
        } else if character.is_ascii_alphanumeric() || character == '_' {
            let word_start = byte;
            index += 1;
            while index < chars.len()
                && (chars[index].1.is_ascii_alphanumeric() || chars[index].1 == '_')
            {
                index += 1;
            }
            let end = chars.get(index).map_or(code.len(), |(offset, _)| *offset);
            let word = &code[word_start..end];
            if is_keyword(word, language) || word.chars().all(|value| value.is_ascii_digit()) {
                push_plain(&mut spans, &code[start..word_start]);
                let color = if word.chars().all(|value| value.is_ascii_digit()) {
                    Color::Magenta
                } else {
                    Color::Blue
                };
                spans.push(Span::styled(word.to_owned(), Style::default().fg(color)));
                start = end;
            }
        } else {
            index += 1;
        }
    }
    push_plain(&mut spans, &code[start..]);
    spans
}

fn push_plain(spans: &mut Vec<Span<'static>>, text: &str) {
    if !text.is_empty() {
        spans.push(Span::raw(text.to_owned()));
    }
}

fn is_keyword(word: &str, language: Language) -> bool {
    let common = matches!(
        word,
        "break"
            | "case"
            | "class"
            | "const"
            | "continue"
            | "else"
            | "enum"
            | "false"
            | "fn"
            | "for"
            | "function"
            | "if"
            | "import"
            | "let"
            | "match"
            | "new"
            | "null"
            | "return"
            | "static"
            | "struct"
            | "true"
            | "type"
            | "while"
    );
    common
        || matches!(language, Language::Rust)
            && matches!(
                word,
                "as" | "async"
                    | "await"
                    | "crate"
                    | "impl"
                    | "mod"
                    | "move"
                    | "mut"
                    | "pub"
                    | "ref"
                    | "self"
                    | "Self"
                    | "trait"
                    | "unsafe"
                    | "use"
                    | "where"
            )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions_receive_multiple_spans() {
        assert!(
            SyntaxHighlighter::new()
                .line(Path::new("main.rs"), "pub fn main() {}")
                .len()
                > 1
        );
    }

    #[test]
    fn unknown_extensions_remain_plain() {
        assert_eq!(
            SyntaxHighlighter::new()
                .line(Path::new("data.unknown"), "plain")
                .len(),
            1
        );
    }
}
