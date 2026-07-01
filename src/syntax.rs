use std::path::Path;

use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Small tree-sitter integration point used by the TUI today and intended to grow into
/// language-aware diff navigation/highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxSummary {
    pub language: &'static str,
    pub root_kind: String,
    pub has_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageKind {
    Rust,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    Attribute,
    Comment,
    Constant,
    Function,
    Keyword,
    Number,
    Operator,
    Property,
    Punctuation,
    String,
    Type,
    Variable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxSpan {
    pub text: String,
    pub kind: Option<HighlightKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightedLine {
    pub spans: Vec<SyntaxSpan>,
}

const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "function",
    "keyword",
    "number",
    "operator",
    "property",
    "punctuation",
    "string",
    "type",
    "variable",
];

pub fn language_for_path(path: &str) -> Option<LanguageKind> {
    match Path::new(path).extension().and_then(|ext| ext.to_str()) {
        Some("rs") => Some(LanguageKind::Rust),
        _ => None,
    }
}

pub fn summarize(path: &str, source: &str) -> Option<SyntaxSummary> {
    if language_for_path(path) != Some(LanguageKind::Rust) {
        return None;
    }

    let language = tree_sitter_rust::LANGUAGE;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language.into()).ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();
    Some(SyntaxSummary {
        language: "rust",
        root_kind: root.kind().to_owned(),
        has_error: root.has_error(),
    })
}

pub fn highlight(path: &str, source: &str) -> Option<Vec<HighlightedLine>> {
    match language_for_path(path)? {
        LanguageKind::Rust => highlight_rust(source),
    }
    .ok()
}

fn highlight_rust(source: &str) -> Result<Vec<HighlightedLine>, HighlightError> {
    let mut config = HighlightConfiguration::new(
        tree_sitter_rust::LANGUAGE.into(),
        "rust",
        tree_sitter_rust::HIGHLIGHTS_QUERY,
        tree_sitter_rust::INJECTIONS_QUERY,
        "",
    )?;
    config.configure(HIGHLIGHT_NAMES);

    let mut highlighter = Highlighter::new();
    let events = highlighter.highlight(&config, source.as_bytes(), None, |_| None)?;
    highlighted_lines_from_events(source, events)
}

fn highlighted_lines_from_events(
    source: &str,
    events: impl Iterator<Item = Result<HighlightEvent, tree_sitter_highlight::Error>>,
) -> Result<Vec<HighlightedLine>, HighlightError> {
    let mut lines = vec![HighlightedLine { spans: Vec::new() }];
    let mut highlight_stack: Vec<Option<HighlightKind>> = Vec::new();

    for event in events {
        match event? {
            HighlightEvent::Source { start, end } => {
                push_source(
                    &mut lines,
                    &source[start..end],
                    highlight_stack.last().copied().flatten(),
                );
            }
            HighlightEvent::HighlightStart(highlight) => {
                highlight_stack.push(highlight_kind(highlight.0));
            }
            HighlightEvent::HighlightEnd => {
                highlight_stack.pop();
            }
        }
    }

    Ok(lines)
}

fn push_source(lines: &mut Vec<HighlightedLine>, mut source: &str, kind: Option<HighlightKind>) {
    loop {
        let Some((line_prefix, rest)) = source.split_once('\n') else {
            push_span(lines, source, kind);
            break;
        };
        push_span(lines, line_prefix, kind);
        lines.push(HighlightedLine { spans: Vec::new() });
        source = rest;
    }
}

fn push_span(lines: &mut [HighlightedLine], text: &str, kind: Option<HighlightKind>) {
    if text.is_empty() {
        return;
    }
    let line = lines
        .last_mut()
        .expect("highlight output always has a current line");
    if let Some(previous) = line.spans.last_mut()
        && previous.kind == kind
    {
        previous.text.push_str(text);
        return;
    }
    line.spans.push(SyntaxSpan {
        text: text.to_owned(),
        kind,
    });
}

fn highlight_kind(index: usize) -> Option<HighlightKind> {
    Some(match *HIGHLIGHT_NAMES.get(index)? {
        "attribute" => HighlightKind::Attribute,
        "comment" => HighlightKind::Comment,
        "constant" => HighlightKind::Constant,
        "function" => HighlightKind::Function,
        "keyword" => HighlightKind::Keyword,
        "number" => HighlightKind::Number,
        "operator" => HighlightKind::Operator,
        "property" => HighlightKind::Property,
        "punctuation" => HighlightKind::Punctuation,
        "string" => HighlightKind::String,
        "type" => HighlightKind::Type,
        "variable" => HighlightKind::Variable,
        _ => return None,
    })
}

#[derive(Debug)]
enum HighlightError {
    Query,
    Highlight,
}

impl From<tree_sitter::QueryError> for HighlightError {
    fn from(_error: tree_sitter::QueryError) -> Self {
        Self::Query
    }
}

impl From<tree_sitter_highlight::Error> for HighlightError {
    fn from(_error: tree_sitter_highlight::Error) -> Self {
        Self::Highlight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rust_with_tree_sitter() {
        let summary = summarize("src/main.rs", "fn main() {}\n").unwrap();
        assert_eq!(summary.language, "rust");
        assert!(!summary.has_error);
    }

    #[test]
    fn detects_supported_languages_by_path() {
        assert_eq!(language_for_path("src/main.rs"), Some(LanguageKind::Rust));
        assert_eq!(language_for_path("README.md"), None);
    }

    #[test]
    fn highlights_rust_source() {
        let lines = highlight("src/lib.rs", "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        assert_eq!(lines.len(), 4);
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.text == "fn" && span.kind == Some(HighlightKind::Keyword))
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|span| span.text == "\"hi\"" && span.kind == Some(HighlightKind::String))
        );
    }
}
