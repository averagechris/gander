use std::{borrow::Cow, path::Path};

use serde::Deserialize;
use tree_sitter::{Language, Parser};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Hash)]
#[serde(default)]
pub struct SyntaxConfig {
    pub enabled: bool,
    /// Built-in language names that should be active. Empty means no built-ins are active.
    pub languages: Vec<String>,
    /// Extra file detection rules for built-in grammars.
    pub mappings: Vec<SyntaxLanguageMapping>,
    pub theme: SyntaxThemeConfig,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq, Hash)]
#[serde(default)]
pub struct SyntaxLanguageMapping {
    pub name: String,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Hash)]
#[serde(default, rename_all = "kebab-case")]
pub struct SyntaxThemeConfig {
    pub attribute: String,
    pub comment: String,
    pub constant: String,
    pub function: String,
    pub keyword: String,
    pub number: String,
    pub operator: String,
    pub property: String,
    pub punctuation: String,
    pub string: String,
    pub r#type: String,
    pub variable: String,
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

/// A named declaration (function/class/struct/...) found in parsed source,
/// with 0-indexed line bounds into that source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolSpan {
    pub name: String,
    pub kind: &'static str,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightedLine {
    pub spans: Vec<SyntaxSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HighlightOutcome {
    Disabled,
    Unsupported,
    Highlighted {
        language: BuiltinLanguage,
        lines: Vec<HighlightedLine>,
    },
    Failed {
        language: BuiltinLanguage,
    },
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
            theme: SyntaxThemeConfig::default(),
        }
    }
}

impl Default for SyntaxThemeConfig {
    fn default() -> Self {
        Self {
            attribute: "magenta".to_owned(),
            comment: "dark-gray".to_owned(),
            constant: "cyan".to_owned(),
            function: "blue".to_owned(),
            keyword: "magenta bold".to_owned(),
            number: "cyan".to_owned(),
            operator: "gray".to_owned(),
            property: "cyan".to_owned(),
            punctuation: "dark-gray".to_owned(),
            string: "green".to_owned(),
            r#type: "yellow".to_owned(),
            variable: "gray".to_owned(),
        }
    }
}

impl SyntaxThemeConfig {
    pub fn gander_dark() -> Self {
        Self::default()
    }

    pub fn gander_light() -> Self {
        Self {
            attribute: "magenta".to_owned(),
            comment: "dark-gray".to_owned(),
            constant: "blue".to_owned(),
            function: "dark-blue".to_owned(),
            keyword: "magenta bold".to_owned(),
            number: "blue".to_owned(),
            operator: "dark-gray".to_owned(),
            property: "dark-cyan".to_owned(),
            punctuation: "dark-gray".to_owned(),
            string: "dark-green".to_owned(),
            r#type: "dark-yellow".to_owned(),
            variable: "black".to_owned(),
        }
    }

    pub fn monochrome() -> Self {
        Self {
            attribute: "gray".to_owned(),
            comment: "dark-gray italic".to_owned(),
            constant: "white".to_owned(),
            function: "white bold".to_owned(),
            keyword: "white bold".to_owned(),
            number: "white".to_owned(),
            operator: "gray".to_owned(),
            property: "white".to_owned(),
            punctuation: "dark-gray".to_owned(),
            string: "gray".to_owned(),
            r#type: "white".to_owned(),
            variable: "gray".to_owned(),
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

/// Friendly labels for tree-sitter node kinds that declare a named symbol,
/// across the built-in grammars.
fn symbol_kind_label(node_kind: &str) -> Option<&'static str> {
    Some(match node_kind {
        "function_item" | "function_definition" | "function_declaration" => "fn",
        "method_definition" | "method_declaration" => "method",
        "class_definition" | "class_declaration" => "class",
        "struct_item" | "struct_specifier" => "struct",
        "enum_item" | "enum_declaration" => "enum",
        "trait_item" => "trait",
        "impl_item" => "impl",
        "mod_item" => "mod",
        "interface_declaration" => "interface",
        "type_declaration" | "type_item" | "type_alias_declaration" => "type",
        _ => return None,
    })
}

/// Extract named declarations from `source`, in document order.
pub fn symbol_spans(path: &str, source: &str, config: &SyntaxConfig) -> Vec<SymbolSpan> {
    let Some(language) = language_for_path_with_config(path, config) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&language.language()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let mut symbols = Vec::new();
    collect_symbols(tree.root_node(), source.as_bytes(), &mut symbols);
    symbols
}

fn collect_symbols(node: tree_sitter::Node<'_>, source: &[u8], symbols: &mut Vec<SymbolSpan>) {
    if let Some(kind) = symbol_kind_label(node.kind()) {
        let name = if node.kind() == "impl_item" {
            impl_symbol_name(node, source)
        } else {
            node.child_by_field_name("name")
                .or_else(|| node.child_by_field_name("type"))
                .and_then(|name_node| name_node.utf8_text(source).ok())
                .unwrap_or("")
                .to_owned()
        };
        if !name.is_empty() {
            symbols.push(SymbolSpan {
                name,
                kind,
                start_line: node.start_position().row,
                end_line: node.end_position().row,
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_symbols(child, source, symbols);
    }
}

fn impl_symbol_name(node: tree_sitter::Node<'_>, source: &[u8]) -> String {
    let ty = node
        .child_by_field_name("type")
        .and_then(|name_node| name_node.utf8_text(source).ok())
        .unwrap_or("");
    let Some(trait_name) = node
        .child_by_field_name("trait")
        .and_then(|trait_node| trait_node.utf8_text(source).ok())
        .filter(|trait_name| !trait_name.is_empty())
    else {
        return ty.to_owned();
    };
    if ty.is_empty() {
        trait_name.to_owned()
    } else {
        format!("{trait_name} for {ty}")
    }
}

#[cfg(test)]
pub fn highlight(path: &str, source: &str) -> Option<Vec<HighlightedLine>> {
    highlight_with_config(path, source, &SyntaxConfig::default())
}

#[cfg(test)]
pub fn highlight_with_config(
    path: &str,
    source: &str,
    config: &SyntaxConfig,
) -> Option<Vec<HighlightedLine>> {
    match highlight_outcome(path, source, config) {
        HighlightOutcome::Highlighted { lines, .. } => Some(lines),
        HighlightOutcome::Disabled
        | HighlightOutcome::Unsupported
        | HighlightOutcome::Failed { .. } => None,
    }
}

pub fn highlight_outcome(path: &str, source: &str, config: &SyntaxConfig) -> HighlightOutcome {
    if !config.enabled {
        return HighlightOutcome::Disabled;
    }
    let Some(language) = language_for_path_with_config(path, config) else {
        return HighlightOutcome::Unsupported;
    };
    match highlight_language(language, source) {
        Ok(lines) => HighlightOutcome::Highlighted { language, lines },
        Err(_) => HighlightOutcome::Failed { language },
    }
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
    pub fn cache_key(&self) -> String {
        let mappings = self
            .mappings
            .iter()
            .map(|mapping| {
                format!(
                    "{}:{}:{}",
                    mapping.name,
                    mapping.extensions.join(","),
                    mapping.filenames.join(",")
                )
            })
            .collect::<Vec<_>>()
            .join(";");
        format!(
            "enabled={};languages={};mappings={}",
            self.enabled,
            self.languages.join(","),
            mappings
        )
    }

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
        assert_eq!(
            highlight_outcome("src/main.rs", "fn main() {}\n", &config),
            HighlightOutcome::Disabled
        );
    }

    #[test]
    fn unsupported_syntax_reports_unsupported_outcome() {
        assert_eq!(
            highlight_outcome("README.unknown", "plain text\n", &SyntaxConfig::default()),
            HighlightOutcome::Unsupported
        );
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
    fn extracts_rust_symbols_with_line_spans() {
        let source = "struct Point {\n    x: i32,\n}\n\nimpl Point {\n    fn new() -> Self {\n        todo!()\n    }\n}\n";

        let symbols = symbol_spans("src/lib.rs", source, &SyntaxConfig::default());

        assert!(symbols.contains(&SymbolSpan {
            name: "Point".to_owned(),
            kind: "struct",
            start_line: 0,
            end_line: 2,
        }));
        assert!(symbols.contains(&SymbolSpan {
            name: "Point".to_owned(),
            kind: "impl",
            start_line: 4,
            end_line: 8,
        }));
        assert!(symbols.contains(&SymbolSpan {
            name: "new".to_owned(),
            kind: "fn",
            start_line: 5,
            end_line: 7,
        }));
    }

    #[test]
    fn labels_trait_impls_with_trait_and_type() {
        let source = "enum Priority { High }\n\nimpl Default for Priority {\n    fn default() -> Self { Self::High }\n}\n\nimpl Priority {\n    fn rank(&self) -> u8 { 1 }\n}\n";

        let symbols = symbol_spans("src/lib.rs", source, &SyntaxConfig::default());

        assert!(symbols.contains(&SymbolSpan {
            name: "Default for Priority".to_owned(),
            kind: "impl",
            start_line: 2,
            end_line: 4,
        }));
        assert!(symbols.contains(&SymbolSpan {
            name: "Priority".to_owned(),
            kind: "impl",
            start_line: 6,
            end_line: 8,
        }));
    }

    #[test]
    fn extracts_symbols_across_languages() {
        let cases: [(&str, &str, &str, &str); 3] = [
            (
                "app.py",
                "class Widget:\n    def draw(self):\n        pass\n",
                "class",
                "Widget",
            ),
            ("main.go", "package main\n\nfunc run() {}\n", "fn", "run"),
            ("app.ts", "export function main(): void {}\n", "fn", "main"),
        ];

        for (path, source, kind, name) in cases {
            let symbols = symbol_spans(path, source, &SyntaxConfig::default());
            assert!(
                symbols
                    .iter()
                    .any(|symbol| symbol.kind == kind && symbol.name == name),
                "expected {kind} {name} in {path}, got {symbols:?}"
            );
        }
    }

    #[test]
    fn unsupported_or_disabled_syntax_yields_no_symbols() {
        assert!(
            symbol_spans("notes.unknown", "fn main() {}\n", &SyntaxConfig::default()).is_empty()
        );
        let disabled = SyntaxConfig {
            enabled: false,
            ..SyntaxConfig::default()
        };
        assert!(symbol_spans("src/lib.rs", "fn main() {}\n", &disabled).is_empty());
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
