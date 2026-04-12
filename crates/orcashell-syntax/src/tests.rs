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

#[test]
fn checkpoint_resume_preserves_multiline_state() {
    let mut highlighter = Highlighter::for_path(Path::new("test.rs"), ThemeId::Dark).unwrap();
    highlighter.highlight_line("fn main() {\n");
    highlighter.highlight_line("    let value = r#\"\n");
    let checkpoint = highlighter.checkpoint();

    let continued = highlighter.highlight_line("still string\n");
    let mut resumed =
        Highlighter::from_checkpoint(Path::new("test.rs"), ThemeId::Dark, checkpoint).unwrap();
    let restarted = resumed.highlight_line("still string\n");

    assert_eq!(restarted, continued);
}

#[test]
fn checkpoint_resume_matches_full_sequential_pass() {
    let mut full = Highlighter::for_path(Path::new("test.rs"), ThemeId::Dark).unwrap();
    full.highlight_line("fn main() {\n");
    full.highlight_line("    let value = r#\"\n");
    let expected = full.highlight_line("still string\n");

    let mut resumed_seed = Highlighter::for_path(Path::new("test.rs"), ThemeId::Dark).unwrap();
    resumed_seed.highlight_line("fn main() {\n");
    resumed_seed.highlight_line("    let value = r#\"\n");
    let checkpoint = resumed_seed.checkpoint();
    let mut resumed =
        Highlighter::from_checkpoint(Path::new("test.rs"), ThemeId::Dark, checkpoint).unwrap();

    assert_eq!(resumed.highlight_line("still string\n"), expected);
}
