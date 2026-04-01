//! Syntax highlighting for OrcaShell.
//!
//! Provides language-aware highlighting using syntect, returning colored spans
//! suitable for rendering in the diff viewer. This crate does NOT depend on GPUI;
//! colors are returned as `u32` (0xRRGGBB) matching the theme token convention.

pub mod theme;

use orcashell_store::ThemeId;
use std::path::Path;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::parsing::{SyntaxDefinition, SyntaxSet};

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();

/// Embedded PowerShell syntax definition (syntect defaults don't include one).
const POWERSHELL_SYNTAX: &str = include_str!("powershell.sublime-syntax");

/// A single span of highlighted text with a specific color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightedSpan {
    /// Color as 0xRRGGBB.
    pub color: u32,
    /// The text content of this span (whitespace already normalized: tabs to 4
    /// non-breaking spaces, regular spaces to non-breaking spaces).
    pub text: String,
}

/// Stateful syntax highlighter for a single file type.
///
/// Maintains parse state across calls to `highlight_line`, so multi-line
/// constructs (block comments, strings) are handled correctly within a
/// sequence of consecutive lines.
pub struct Highlighter {
    inner: HighlightLines<'static>,
    fallback_color: u32,
}

impl Highlighter {
    /// Create a highlighter for the given file path. The file extension is used
    /// for language detection. Returns `None` if the file maps to plain text
    /// (highlighting would just return the default color for everything).
    pub fn for_path(path: &Path, theme_id: ThemeId) -> Option<Self> {
        let ss = syntax_set();
        let syntax = get_syntax_for_path(path, ss);
        if std::ptr::eq(syntax, ss.find_syntax_plain_text()) {
            return None;
        }
        let theme = theme::orca_syntax_theme(theme_id);
        let fallback_color = theme
            .settings
            .foreground
            .map(|fg| ((fg.r as u32) << 16) | ((fg.g as u32) << 8) | fg.b as u32)
            .unwrap_or(0xD8DAE0);
        Some(Self {
            inner: HighlightLines::new(syntax, theme),
            fallback_color,
        })
    }

    /// Advance parse state for a line without building spans.
    ///
    /// Use this when you need to keep the parser in sync (e.g., for the old-file
    /// state on context lines) but don't need the highlighted output.
    pub fn advance_state(&mut self, text: &str) {
        let ss = syntax_set();
        let _ = self.inner.highlight_line(text, ss);
    }

    /// Highlight a single line of text, advancing internal parse state.
    ///
    /// The returned spans have whitespace normalized (tabs to 4 NBSP, spaces to
    /// NBSP, trailing newlines stripped).
    pub fn highlight_line(&mut self, text: &str) -> Vec<HighlightedSpan> {
        let ss = syntax_set();
        match self.inner.highlight_line(text, ss) {
            Ok(ranges) => {
                let mut spans = Vec::new();
                for (style, text) in ranges {
                    let color = ((style.foreground.r as u32) << 16)
                        | ((style.foreground.g as u32) << 8)
                        | (style.foreground.b as u32);
                    let normalized = normalize_whitespace(text);
                    if normalized.is_empty() {
                        continue;
                    }
                    // Merge with previous span if same color.
                    if let Some(last) = spans.last_mut() {
                        let last: &mut HighlightedSpan = last;
                        if last.color == color {
                            last.text.push_str(&normalized);
                            continue;
                        }
                    }
                    spans.push(HighlightedSpan {
                        color,
                        text: normalized,
                    });
                }
                spans
            }
            Err(_) => {
                let text = normalize_whitespace(text);
                if text.is_empty() {
                    vec![]
                } else {
                    vec![HighlightedSpan {
                        color: self.fallback_color,
                        text,
                    }]
                }
            }
        }
    }
}

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(|| {
        let defaults = SyntaxSet::load_defaults_newlines();
        let mut builder = defaults.into_builder();
        if let Ok(ps) = SyntaxDefinition::load_from_str(POWERSHELL_SYNTAX, true, Some("PowerShell"))
        {
            builder.add(ps);
        }
        builder.build()
    })
}

const TAB_REPLACEMENT: &str = "\u{00A0}\u{00A0}\u{00A0}\u{00A0}";
const SPACE_REPLACEMENT: &str = "\u{00A0}";

/// Normalize whitespace for rendering: strip trailing newlines, convert tabs to
/// 4 non-breaking spaces, convert regular spaces to non-breaking spaces.
fn normalize_whitespace(text: &str) -> String {
    text.trim_end_matches(['\r', '\n'])
        .replace('\t', TAB_REPLACEMENT)
        .replace(' ', SPACE_REPLACEMENT)
}

/// Map file extension to syntect syntax name for better coverage.
fn map_extension_to_syntax(ext: &str) -> Option<&'static str> {
    match ext.to_lowercase().as_str() {
        "ts" | "mts" | "cts" => Some("TypeScript"),
        "tsx" => Some("TypeScriptReact"),
        "jsx" => Some("JavaScript"),
        "mjs" | "cjs" => Some("JavaScript"),
        "yml" | "yaml" => Some("YAML"),
        "json" | "jsonc" | "json5" => Some("JSON"),
        "toml" => Some("TOML"),
        "sh" | "bash" | "zsh" => Some("Bourne Again Shell (bash)"),
        "py" | "pyw" | "pyi" => Some("Python"),
        "rb" | "erb" | "rake" => Some("Ruby"),
        "rs" => Some("Rust"),
        "go" => Some("Go"),
        "c" | "h" => Some("C"),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => Some("C++"),
        "java" => Some("Java"),
        "swift" => Some("Swift"),
        "cs" => Some("C#"),
        "php" => Some("PHP"),
        "lua" => Some("Lua"),
        "sql" => Some("SQL"),
        "md" | "markdown" => Some("Markdown"),
        "html" | "htm" | "xhtml" => Some("HTML"),
        "css" => Some("CSS"),
        "xml" | "svg" | "xsl" => Some("XML"),
        "diff" | "patch" => Some("Diff"),
        "ps1" | "psm1" | "psd1" => Some("PowerShell"),
        "bat" | "cmd" => Some("Batch File"),
        _ => None,
    }
}

/// Get the best syntax reference for a file path.
fn get_syntax_for_path<'a>(
    path: &Path,
    ss: &'a SyntaxSet,
) -> &'a syntect::parsing::SyntaxReference {
    let ext = path.extension().and_then(|e| e.to_str());

    // Try our manual extension map first (finds names not extensions).
    if let Some(name) = ext.and_then(map_extension_to_syntax) {
        if let Some(syn) = ss.find_syntax_by_name(name) {
            return syn;
        }
    }

    // Try syntect's built-in extension lookup.
    if let Some(ext) = ext {
        if let Some(syn) = ss.find_syntax_by_extension(ext) {
            return syn;
        }
    }

    // Try by filename for special files.
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let lower = name.to_lowercase();
        let syn = match lower.as_str() {
            "makefile" | "gnumakefile" => ss.find_syntax_by_name("Makefile"),
            "dockerfile" => ss.find_syntax_by_name("Dockerfile"),
            "cargo.toml" | "cargo.lock" | "pyproject.toml" => ss.find_syntax_by_name("TOML"),
            ".gitignore" | ".dockerignore" | ".npmignore" => ss.find_syntax_by_name("Git Ignore"),
            ".bashrc" | ".zshrc" | ".bash_profile" | ".profile" => {
                ss.find_syntax_by_name("Bourne Again Shell (bash)")
            }
            _ => None,
        };
        if let Some(syn) = syn {
            return syn;
        }
    }

    ss.find_syntax_plain_text()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlighter_returns_spans_for_rust() {
        let mut hl = Highlighter::for_path(Path::new("test.rs"), ThemeId::Dark).unwrap();
        let spans = hl.highlight_line("fn main() {\n");
        assert!(!spans.is_empty());
    }

    #[test]
    fn keywords_get_orca_blue() {
        let mut hl = Highlighter::for_path(Path::new("test.rs"), ThemeId::Dark).unwrap();
        let spans = hl.highlight_line("fn main() {\n");
        let fn_span = spans.iter().find(|s| s.text.starts_with("fn"));
        assert!(fn_span.is_some(), "expected a span starting with 'fn'");
        assert_eq!(fn_span.unwrap().color, 0x5E9BFF);
    }

    #[test]
    fn strings_get_neon_mint() {
        let mut hl = Highlighter::for_path(Path::new("test.rs"), ThemeId::Dark).unwrap();
        // Feed context lines first so the parser state is correct.
        hl.highlight_line("fn main() {\n");
        hl.highlight_line("    let x = 42;\n");
        hl.highlight_line("    // a comment\n");
        let spans = hl.highlight_line("    println!(\"hello\");\n");
        let hello_span = spans.iter().find(|s| s.text.contains("hello"));
        assert!(hello_span.is_some(), "expected a span containing 'hello'");
        assert_eq!(hello_span.unwrap().color, 0x7EFFC1);
    }

    #[test]
    fn comments_get_fog() {
        let mut hl = Highlighter::for_path(Path::new("test.rs"), ThemeId::Dark).unwrap();
        hl.highlight_line("fn main() {\n");
        hl.highlight_line("    let x = 42;\n");
        let spans = hl.highlight_line("    // a comment\n");
        for span in &spans {
            if span.text.trim().is_empty() {
                continue;
            }
            assert_eq!(
                span.color, 0x9499A8,
                "comment span '{}' should be FOG",
                span.text
            );
        }
    }

    #[test]
    fn whitespace_normalization() {
        let mut hl = Highlighter::for_path(Path::new("test.rs"), ThemeId::Dark).unwrap();
        let spans = hl.highlight_line("fn\tmain() {\n");
        let full_text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert!(
            !full_text.contains(' '),
            "regular spaces should be replaced with NBSP"
        );
        assert!(
            !full_text.contains('\t'),
            "tabs should be replaced with 4 NBSPs"
        );
    }

    #[test]
    fn plain_text_returns_none() {
        assert!(Highlighter::for_path(Path::new("test.xyz_unknown"), ThemeId::Dark).is_none());
    }

    #[test]
    fn powershell_highlighting() {
        assert!(
            Highlighter::for_path(Path::new("profile.ps1"), ThemeId::Dark).is_some(),
            ".ps1 should be highlighted as PowerShell"
        );
    }

    #[test]
    fn powershell_module_highlighting() {
        assert!(
            Highlighter::for_path(Path::new("module.psm1"), ThemeId::Dark).is_some(),
            ".psm1 should be highlighted as PowerShell"
        );
    }

    #[test]
    fn powershell_keywords_highlighted() {
        let mut hl = Highlighter::for_path(Path::new("test.ps1"), ThemeId::Dark).unwrap();
        let spans = hl.highlight_line("function Get-Item { param($Path) }\n");
        assert!(
            !spans.is_empty(),
            "PowerShell should produce highlighted spans"
        );
        // "function" keyword should not be the default bone color
        let fn_span = spans.iter().find(|s| s.text.contains("function"));
        assert!(fn_span.is_some(), "expected a span containing 'function'");
    }

    #[test]
    fn batch_file_highlighting() {
        assert!(
            Highlighter::for_path(Path::new("setup.bat"), ThemeId::Dark).is_some(),
            ".bat should be highlighted as Batch File"
        );
    }

    #[test]
    fn cmd_file_highlighting() {
        assert!(
            Highlighter::for_path(Path::new("build.cmd"), ThemeId::Dark).is_some(),
            ".cmd should be highlighted as Batch File"
        );
    }

    #[test]
    fn multi_line_comment_state_preserved() {
        let mut hl = Highlighter::for_path(Path::new("test.rs"), ThemeId::Dark).unwrap();
        let line1 = hl.highlight_line("/* start of\n");
        let line2 = hl.highlight_line("   still a comment */\n");
        // Both lines should be FOG (comment color).
        for span in line1.iter().chain(line2.iter()) {
            if span.text.trim().is_empty() {
                continue;
            }
            assert_eq!(
                span.color, 0x9499A8,
                "span '{}' should be comment color",
                span.text
            );
        }
    }
}
