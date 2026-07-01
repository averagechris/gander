use std::{borrow::Cow, path::Path};

use serde::Deserialize;
use tree_sitter::{Language, Parser};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SyntaxConfig {
    pub enabled: bool,
    /// Built-in language names that should be active. Empty means no built-ins are active.
    pub languages: Vec<String>,
    /// Extra file detection rules for built-in grammars.
    pub mappings: Vec<SyntaxLanguageMapping>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SyntaxLanguageMapping {
    pub name: String,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
}

/// Small tree-sitter integration point used by the TUI today and intended to grow into
/// language-aware diff navigation/highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxSummary {
    pub language: &'static str,
    pub root_kind: String,
    pub has_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinLanguage {
    Bash,
    Css,
    Go,
    Html,
    JavaScript,
    Json,
    Jsx,
    Markdown,
    Nix,
    Python,
    Rust,
    Toml,
    Tsx,
    TypeScript,
    Yaml,
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

const BUILTIN_LANGUAGES: &[BuiltinLanguage] = &[
    BuiltinLanguage::Bash,
    BuiltinLanguage::Css,
    BuiltinLanguage::Go,
    BuiltinLanguage::Html,
    BuiltinLanguage::JavaScript,
    BuiltinLanguage::Json,
    BuiltinLanguage::Jsx,
    BuiltinLanguage::Markdown,
    BuiltinLanguage::Nix,
    BuiltinLanguage::Python,
    BuiltinLanguage::Rust,
    BuiltinLanguage::Toml,
    BuiltinLanguage::Tsx,
    BuiltinLanguage::TypeScript,
    BuiltinLanguage::Yaml,
];

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

impl Default for SyntaxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            languages: BuiltinLanguage::all_names()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            mappings: Vec::new(),
        }
    }
}

impl BuiltinLanguage {
    pub fn all_names() -> Vec<&'static str> {
        BUILTIN_LANGUAGES
            .iter()
            .map(|language| language.name())
            .collect()
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Css => "css",
            Self::Go => "go",
            Self::Html => "html",
            Self::JavaScript => "javascript",
            Self::Json => "json",
            Self::Jsx => "jsx",
            Self::Markdown => "markdown",
            Self::Nix => "nix",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Toml => "toml",
            Self::Tsx => "tsx",
            Self::TypeScript => "typescript",
            Self::Yaml => "yaml",
        }
    }

    const fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Bash => &["bash", "bats", "sh", "zsh"],
            Self::Css => &["css"],
            Self::Go => &["go"],
            Self::Html => &["htm", "html"],
            Self::JavaScript => &["cjs", "js", "mjs"],
            Self::Json => &["json", "jsonc"],
            Self::Jsx => &["jsx"],
            Self::Markdown => &["markdown", "md", "mdown", "mkd"],
            Self::Nix => &["nix"],
            Self::Python => &["py", "pyi", "pyw"],
            Self::Rust => &["rs"],
            Self::Toml => &["toml"],
            Self::Tsx => &["tsx"],
            Self::TypeScript => &["cts", "mts", "ts"],
            Self::Yaml => &["yaml", "yml"],
        }
    }

    const fn filenames(self) -> &'static [&'static str] {
        match self {
            Self::Bash => &[".bashrc", ".envrc", ".profile", ".zshrc"],
            _ => &[],
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        BUILTIN_LANGUAGES
            .iter()
            .copied()
            .find(|language| language.name() == name)
    }

    fn language(self) -> Language {
        match self {
            Self::Bash => tree_sitter_bash::LANGUAGE.into(),
            Self::Css => tree_sitter_css::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Html => tree_sitter_html::LANGUAGE.into(),
            Self::JavaScript | Self::Jsx => tree_sitter_javascript::LANGUAGE.into(),
            Self::Json => tree_sitter_json::LANGUAGE.into(),
            Self::Markdown => tree_sitter_md::LANGUAGE.into(),
            Self::Nix => tree_sitter_nix::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Yaml => tree_sitter_yaml::LANGUAGE.into(),
        }
    }

    fn highlights_query(self) -> Cow<'static, str> {
        match self {
            Self::Bash => Cow::Borrowed(tree_sitter_bash::HIGHLIGHT_QUERY),
            Self::Css => Cow::Borrowed(tree_sitter_css::HIGHLIGHTS_QUERY),
            Self::Go => Cow::Borrowed(tree_sitter_go::HIGHLIGHTS_QUERY),
            Self::Html => Cow::Borrowed(tree_sitter_html::HIGHLIGHTS_QUERY),
            Self::JavaScript => Cow::Borrowed(tree_sitter_javascript::HIGHLIGHT_QUERY),
            Self::Json => Cow::Borrowed(tree_sitter_json::HIGHLIGHTS_QUERY),
            Self::Jsx => Cow::Owned(format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
            )),
            Self::Markdown => Cow::Borrowed(tree_sitter_md::HIGHLIGHT_QUERY_BLOCK),
            Self::Nix => Cow::Borrowed(tree_sitter_nix::HIGHLIGHTS_QUERY),
            Self::Python => Cow::Borrowed(tree_sitter_python::HIGHLIGHTS_QUERY),
            Self::Rust => Cow::Borrowed(tree_sitter_rust::HIGHLIGHTS_QUERY),
            Self::Toml => Cow::Borrowed(tree_sitter_toml_ng::HIGHLIGHTS_QUERY),
            Self::Tsx | Self::TypeScript => Cow::Borrowed(tree_sitter_typescript::HIGHLIGHTS_QUERY),
            Self::Yaml => Cow::Borrowed(tree_sitter_yaml::HIGHLIGHTS_QUERY),
        }
    }

    fn injections_query(self) -> &'static str {
        match self {
            Self::Html => tree_sitter_html::INJECTIONS_QUERY,
            Self::JavaScript | Self::Jsx => tree_sitter_javascript::INJECTIONS_QUERY,
            Self::Markdown => tree_sitter_md::INJECTION_QUERY_BLOCK,
            Self::Nix => tree_sitter_nix::INJECTIONS_QUERY,
            Self::Rust => tree_sitter_rust::INJECTIONS_QUERY,
            _ => "",
        }
    }

    fn locals_query(self) -> &'static str {
        match self {
            Self::JavaScript | Self::Jsx => tree_sitter_javascript::LOCALS_QUERY,
            Self::Tsx | Self::TypeScript => tree_sitter_typescript::LOCALS_QUERY,
            _ => "",
        }
    }
}

#[cfg(test)]
pub fn language_for_path(path: &str) -> Option<BuiltinLanguage> {
    language_for_path_with_config(path, &SyntaxConfig::default())
}

pub fn language_for_path_with_config(path: &str, config: &SyntaxConfig) -> Option<BuiltinLanguage> {
    if !config.enabled {
        return None;
    }

    BUILTIN_LANGUAGES
        .iter()
        .copied()
        .find(|language| config.language_enabled(language.name()) && language.matches_path(path))
        .or_else(|| configured_language_for_path(path, config))
}

#[cfg(test)]
pub fn summarize(path: &str, source: &str) -> Option<SyntaxSummary> {
    summarize_with_config(path, source, &SyntaxConfig::default())
}

pub fn summarize_with_config(
    path: &str,
    source: &str,
    config: &SyntaxConfig,
) -> Option<SyntaxSummary> {
    let language = language_for_path_with_config(path, config)?;

    let mut parser = Parser::new();
    parser.set_language(&language.language()).ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();
    Some(SyntaxSummary {
        language: language.name(),
        root_kind: root.kind().to_owned(),
        has_error: root.has_error(),
    })
}

#[cfg(test)]
pub fn highlight(path: &str, source: &str) -> Option<Vec<HighlightedLine>> {
    highlight_with_config(path, source, &SyntaxConfig::default())
}

pub fn highlight_with_config(
    path: &str,
    source: &str,
    config: &SyntaxConfig,
) -> Option<Vec<HighlightedLine>> {
    let language = language_for_path_with_config(path, config)?;
    highlight_language(language, source).ok()
}

fn highlight_language(
    language: BuiltinLanguage,
    source: &str,
) -> Result<Vec<HighlightedLine>, HighlightError> {
    let highlights = language.highlights_query();
    let mut config = HighlightConfiguration::new(
        language.language(),
        language.name(),
        &highlights,
        language.injections_query(),
        language.locals_query(),
    )?;
    config.configure(HIGHLIGHT_NAMES);

    let mut highlighter = Highlighter::new();
    let events = highlighter.highlight(&config, source.as_bytes(), None, |_| None)?;
    highlighted_lines_from_events(source, events)
}

impl BuiltinLanguage {
    fn matches_path(self, path: &str) -> bool {
        let path = Path::new(path);
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        if extension
            .as_deref()
            .is_some_and(|extension| self.extensions().contains(&extension))
        {
            return true;
        }

        path.file_name()
            .and_then(|filename| filename.to_str())
            .is_some_and(|filename| self.filenames().contains(&filename))
    }
}

impl SyntaxConfig {
    fn language_enabled(&self, name: &str) -> bool {
        self.languages.iter().any(|language| language == name)
    }
}

fn configured_language_for_path(path: &str, config: &SyntaxConfig) -> Option<BuiltinLanguage> {
    let path = Path::new(path);
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let filename = path.file_name().and_then(|filename| filename.to_str());

    config.mappings.iter().find_map(|mapping| {
        let language = BuiltinLanguage::from_name(&mapping.name)?;
        if !config.language_enabled(language.name()) {
            return None;
        }
        let extension_matches = extension.as_deref().is_some_and(|extension| {
            mapping
                .extensions
                .iter()
                .any(|configured| configured.eq_ignore_ascii_case(extension))
        });
        let filename_matches = filename.is_some_and(|filename| {
            mapping
                .filenames
                .iter()
                .any(|configured| configured == filename)
        });
        (extension_matches || filename_matches).then_some(language)
    })
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
        assert_eq!(
            language_for_path("src/main.rs"),
            Some(BuiltinLanguage::Rust)
        );
        assert_eq!(
            language_for_path("src/app.ts"),
            Some(BuiltinLanguage::TypeScript)
        );
        assert_eq!(language_for_path("src/app.tsx"), Some(BuiltinLanguage::Tsx));
        assert_eq!(language_for_path("flake.nix"), Some(BuiltinLanguage::Nix));
        assert_eq!(language_for_path("README.unknown"), None);
    }

    #[test]
    fn supports_configured_language_mappings() {
        let config = SyntaxConfig {
            mappings: vec![SyntaxLanguageMapping {
                name: "python".to_owned(),
                extensions: vec!["custompy".to_owned()],
                filenames: vec!["SConstruct".to_owned()],
            }],
            ..SyntaxConfig::default()
        };

        assert_eq!(
            language_for_path_with_config("build.custompy", &config),
            Some(BuiltinLanguage::Python)
        );
        assert_eq!(
            language_for_path_with_config("SConstruct", &config),
            Some(BuiltinLanguage::Python)
        );
    }

    #[test]
    fn disabled_syntax_matches_no_languages() {
        let config = SyntaxConfig {
            enabled: false,
            ..SyntaxConfig::default()
        };

        assert_eq!(language_for_path_with_config("src/main.rs", &config), None);
        assert!(highlight_with_config("src/main.rs", "fn main() {}\n", &config).is_none());
    }

    #[test]
    fn language_allow_list_limits_detection() {
        let config = SyntaxConfig {
            languages: vec!["python".to_owned()],
            ..SyntaxConfig::default()
        };

        assert_eq!(language_for_path_with_config("src/main.rs", &config), None);
        assert_eq!(
            language_for_path_with_config("script.py", &config),
            Some(BuiltinLanguage::Python)
        );
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

    #[test]
    fn highlights_common_languages() {
        let cases = [
            ("script.py", "def main():\n    return 1\n"),
            ("app.ts", "function main(): number { return 1; }\n"),
            ("main.go", "package main\nfunc main() {}\n"),
            ("data.json", "{\"ok\": true}\n"),
            ("flake.nix", "{ pkgs }: pkgs.hello\n"),
        ];

        for (path, source) in cases {
            assert!(
                highlight(path, source).is_some(),
                "expected highlights for {path}"
            );
        }
    }
}
