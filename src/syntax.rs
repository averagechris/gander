use std::path::Path;

/// Small tree-sitter integration point used by the TUI today and intended to grow into
/// language-aware diff navigation/highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxSummary {
    pub language: &'static str,
    pub root_kind: String,
    pub has_error: bool,
}

pub fn summarize(path: &str, source: &str) -> Option<SyntaxSummary> {
    if Path::new(path).extension().and_then(|ext| ext.to_str()) != Some("rs") {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rust_with_tree_sitter() {
        let summary = summarize("src/main.rs", "fn main() {}\n").unwrap();
        assert_eq!(summary.language, "rust");
        assert!(!summary.has_error);
    }
}
