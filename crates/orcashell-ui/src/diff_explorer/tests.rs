use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use super::{
    build_conflict_render_lines, build_conflict_render_lines_with_cache, build_diff_tree,
    build_stash_line_cache, collect_file_keys, conflict_block_navigation_disabled,
    create_stash_prompt_spec, delete_document_backward, delete_document_forward, diff_line_colors,
    discard_all_prompt_spec, extract_selected_text, format_relative_time_from,
    indent_document_selection, is_copy_keystroke, is_displayed_file_stale, is_oversize_document,
    line_cache_matches, map_raw_to_display_ranges, max_diff_content_width,
    navigated_conflict_block_index, normalize_display_ranges, outdent_document_selection,
    plain_text_for_line, render_diff_text_string, replace_document_range, selection_range_for_line,
    stash_display_title, stash_header_button_label, stash_line_cache_key, stash_line_cache_matches,
    CachedConflictLines, ConflictEditorDocument, DiffRenderSnapshot, DiffSelection,
    DiffTreeNodeKind,
};
use crate::prompt_dialog::PromptDialogConfirmTone;
use crate::settings::ThemeId;
use crate::workspace::{
    DiffFileState, DiffIndexState, DiffTabStashState, DiffTabState, DiffTabViewMode,
};
use gpui::Modifiers;
use orcashell_git::{
    parse_conflict_file_text, ChangedFile, DiffDocument, DiffLineKind, DiffLineView,
    DiffSectionKind, DiffSelectionKey, FileDiffDocument, GitFileStatus, GitSnapshotSummary,
    GitTrackingStatus, MergeState, Oid, StashFileDiffDocument, StashFileSelection,
    OVERSIZE_DIFF_MESSAGE,
};

fn file(path: &str, status: GitFileStatus) -> ChangedFile {
    ChangedFile {
        relative_path: PathBuf::from(path),
        status,
        is_binary: false,
        insertions: 1,
        deletions: 0,
    }
}

fn diff_line(text: &str, kind: DiffLineKind) -> DiffLineView {
    DiffLineView {
        kind,
        old_lineno: None,
        new_lineno: None,
        text: text.to_string(),
        highlights: None,
        inline_changes: None,
    }
}

fn oid(value: u64) -> Oid {
    Oid::from_str(&format!("{value:040x}")).unwrap()
}

fn stash_selection(path: &str) -> StashFileSelection {
    StashFileSelection {
        stash_oid: oid(7),
        relative_path: PathBuf::from(path),
    }
}

fn stash_file_document(path: &str, lines: Vec<DiffLineView>) -> StashFileDiffDocument {
    StashFileDiffDocument {
        stash_oid: oid(7),
        selection: stash_selection(path),
        file: file(path, GitFileStatus::Modified),
        lines,
    }
}

fn cache_for_conflict_text(text: &str, path: &str) -> CachedConflictLines {
    let selection = DiffSelectionKey {
        section: DiffSectionKind::Conflicted,
        relative_path: PathBuf::from(path),
    };
    let (lines, anchors) = build_conflict_render_lines_with_cache(
        text,
        PathBuf::from(path).as_path(),
        ThemeId::Dark,
        None,
    );
    CachedConflictLines {
        selection,
        generation: 1,
        version: 1,
        theme_id: ThemeId::Dark,
        raw_text: text.to_string(),
        lines: Rc::new(lines),
        anchors: Rc::new(anchors),
        max_line_chars: 0,
    }
}

fn conflict_line_fingerprint(
    lines: &[super::ConflictRenderLine],
) -> Vec<(
    DiffLineKind,
    Option<Vec<(u32, String)>>,
    super::ConflictHighlightMode,
)> {
    lines
        .iter()
        .map(|line| {
            (
                line.kind,
                line.highlights.as_ref().map(|spans| {
                    spans
                        .iter()
                        .map(|span| (span.color, span.text.clone()))
                        .collect()
                }),
                line.highlight_mode,
            )
        })
        .collect()
}

fn snapshot(scope_root: &str, generation: u64) -> GitSnapshotSummary {
    let scope_root = PathBuf::from(scope_root);
    GitSnapshotSummary {
        repo_root: scope_root.clone(),
        scope_root,
        generation,
        content_fingerprint: generation,
        branch_name: "main".into(),
        remotes: vec!["origin".into()],
        is_worktree: false,
        worktree_name: None,
        changed_files: 1,
        insertions: 1,
        deletions: 0,
    }
}

fn diff_tab_with_document(
    scope_root: &str,
    generation: u64,
    document: DiffDocument,
    selected_file: Option<DiffSelectionKey>,
) -> DiffTabState {
    DiffTabState {
        scope_root: PathBuf::from(scope_root),
        tree_width: 300.0,
        view_mode: DiffTabViewMode::WorkingTree,
        index: DiffIndexState {
            document: Some(document),
            error: None,
            loading: false,
            requested_generation: Some(generation),
        },
        selected_file,
        file: DiffFileState::default(),
        stash: DiffTabStashState::default(),
        conflict_editor: Default::default(),
        multi_select: HashSet::new(),
        selection_anchor: None,
        commit_message: String::new(),
        local_action_in_flight: false,
        remote_op_in_flight: false,
        last_action_banner: None,
        remove_worktree_confirm: None,
        managed_worktree: None,
    }
}

#[test]
fn build_diff_tree_groups_directories_before_files() {
    let tree = build_diff_tree(&[
        file("src/lib.rs", GitFileStatus::Modified),
        file("src/app/mod.rs", GitFileStatus::Added),
        file("README.md", GitFileStatus::Modified),
    ]);

    assert_eq!(tree.len(), 2);
    assert!(matches!(tree[0].kind, DiffTreeNodeKind::Directory));
    assert!(matches!(tree[1].kind, DiffTreeNodeKind::File(_)));
    assert_eq!(tree[0].name, "src");
    assert_eq!(tree[1].name, "README.md");
}

#[test]
fn render_snapshot_counts_conflicts_and_uses_selected_conflict_meta() {
    let selected = DiffSelectionKey {
        section: DiffSectionKind::Conflicted,
        relative_path: PathBuf::from("src/conflict.rs"),
    };
    let tab = diff_tab_with_document(
        "/tmp/repo",
        7,
        DiffDocument {
            snapshot: snapshot("/tmp/repo", 7),
            tracking: GitTrackingStatus {
                upstream_ref: None,
                ahead: 0,
                behind: 0,
            },
            merge_state: Some(MergeState {
                can_complete: false,
                can_abort: true,
                conflicted_file_count: 1,
            }),
            repo_state_warning: None,
            conflicted_files: vec![file("src/conflict.rs", GitFileStatus::Conflicted)],
            staged_files: Vec::new(),
            unstaged_files: Vec::new(),
        },
        Some(selected.clone()),
    );

    let snapshot = DiffRenderSnapshot::from_tab(&tab);

    assert_eq!(snapshot.index_file_count, 1);
    assert_eq!(snapshot.conflicted_file_count, 1);
    assert_eq!(
        snapshot.file_meta.map(|file| file.relative_path),
        Some(selected.relative_path)
    );
}

#[test]
fn merge_state_keeps_staged_section_visible_but_hides_commit_input() {
    let tab = diff_tab_with_document(
        "/tmp/repo",
        7,
        DiffDocument {
            snapshot: snapshot("/tmp/repo", 7),
            tracking: GitTrackingStatus {
                upstream_ref: None,
                ahead: 0,
                behind: 0,
            },
            merge_state: Some(MergeState {
                can_complete: false,
                can_abort: true,
                conflicted_file_count: 1,
            }),
            repo_state_warning: None,
            conflicted_files: vec![file("src/conflict.rs", GitFileStatus::Conflicted)],
            staged_files: vec![file("src/resolved.rs", GitFileStatus::Modified)],
            unstaged_files: vec![file("README.md", GitFileStatus::Modified)],
        },
        None,
    );

    let snapshot = DiffRenderSnapshot::from_tab(&tab);

    assert_eq!(snapshot.conflicted_file_count, 1);
    assert_eq!(snapshot.staged_file_count, 1);
    assert_eq!(snapshot.unstaged_file_count, 1);
    assert!(!snapshot.show_commit_input());
    assert!(snapshot.show_staged_section());
}

#[test]
fn render_diff_text_preserves_spaces() {
    assert_eq!(
        render_diff_text_string("a b\tc"),
        "a\u{00A0}b\u{00A0}\u{00A0}\u{00A0}\u{00A0}c"
    );
}

#[test]
fn render_diff_text_trims_trailing_newlines() {
    assert_eq!(render_diff_text_string("line\n"), "line");
    assert_eq!(render_diff_text_string("line\r\n"), "line");
}

#[test]
fn map_raw_to_display_ranges_with_spaces_and_tabs() {
    // "a b\tc\n" → display: "a" NBSP "b" NBSP×4 "c"
    // raw bytes:  a=0, ' '=1, b=2, '\t'=3, c=4, '\n'=5
    // display:    a=0, NBSP=1..3, b=3, NBSP×4=4..12, c=12
    let raw = "a b\tc\n";
    let ranges = vec![2..5]; // raw "b\tc"
    let mapped = map_raw_to_display_ranges(raw, &ranges);
    // display "b" starts at 3, display "c" ends at 13
    assert_eq!(mapped, vec![3..13]);
}

#[test]
fn map_raw_to_display_ranges_empty() {
    let mapped = map_raw_to_display_ranges("hello\n", &[]);
    assert!(mapped.is_empty());
}

#[test]
fn normalize_display_ranges_clamps_invalid_utf8_boundaries() {
    let text = "é🙂x";
    let normalized = normalize_display_ranges(text, vec![1..5, 6..99]);

    assert_eq!(normalized, vec![0..6, 6..7]);
}

#[test]
fn max_diff_content_width_includes_trailing_row_padding() {
    assert_eq!(max_diff_content_width(10, 6.0), 204.0);
}

#[test]
fn oversize_detection_matches_guard_document() {
    let document = FileDiffDocument {
        generation: 3,
        selection: DiffSelectionKey {
            section: DiffSectionKind::Unstaged,
            relative_path: PathBuf::from("Cargo.lock"),
        },
        file: file("Cargo.lock", GitFileStatus::Modified),
        lines: vec![DiffLineView {
            kind: DiffLineKind::BinaryNotice,
            old_lineno: None,
            new_lineno: None,
            text: OVERSIZE_DIFF_MESSAGE.to_string(),
            highlights: None,
            inline_changes: None,
        }],
        hunks: Vec::new(),
    };

    assert!(is_oversize_document(&document));
}

#[test]
fn displayed_file_stale_only_when_live_generation_is_newer() {
    assert!(!is_displayed_file_stale(Some(4), Some(4)));
    assert!(!is_displayed_file_stale(Some(5), Some(4)));
    assert!(!is_displayed_file_stale(Some(4), None));
    assert!(is_displayed_file_stale(Some(4), Some(5)));
}

#[test]
fn discard_all_prompt_is_destructive_and_mentions_unstaged_only() {
    let spec = discard_all_prompt_spec(PathBuf::from("/tmp/repo"));

    assert_eq!(spec.title, "Discard All");
    assert_eq!(spec.confirm_label, "Discard All");
    assert!(matches!(
        spec.confirm_tone,
        PromptDialogConfirmTone::Destructive
    ));
    assert!(spec.detail.as_deref().unwrap().contains("unstaged changes"));
    assert!(spec.detail.as_deref().unwrap().contains("Staged changes"));
}

#[test]
fn create_stash_prompt_allows_empty_message_and_exposes_toggles() {
    let spec = create_stash_prompt_spec(PathBuf::from("/tmp/repo"));

    assert_eq!(spec.title, "Create Stash");
    assert_eq!(spec.confirm_label, "Create Stash");
    assert!(matches!(
        spec.confirm_tone,
        PromptDialogConfirmTone::Primary
    ));
    assert!(spec.input.as_ref().is_some_and(|input| input.allow_empty));
    assert_eq!(spec.toggles.len(), 2);
    assert_eq!(spec.toggles[0].id, "keep_index");
    assert_eq!(spec.toggles[1].id, "include_untracked");
}

#[test]
fn stash_header_button_label_reflects_view_mode() {
    assert_eq!(
        stash_header_button_label(DiffTabViewMode::WorkingTree, 3),
        "Stashes (3)"
    );
    assert_eq!(
        stash_header_button_label(DiffTabViewMode::Stashes, 3),
        "Back to Diff"
    );
}

#[test]
fn stash_display_title_prefers_cleaned_message_text() {
    assert_eq!(
        stash_display_title("On main: polish stash list", "stash@{0}"),
        "polish stash list"
    );
    assert_eq!(
        stash_display_title("WIP on feature/x: abc1234 tighten modal", "stash@{1}"),
        "abc1234 tighten modal"
    );
    assert_eq!(
        stash_display_title("custom user message", "stash@{2}"),
        "custom user message"
    );
}

#[test]
fn stash_display_title_falls_back_to_label_when_cleaned_text_is_empty() {
    assert_eq!(stash_display_title("On main:", "stash@{0}"), "stash@{0}");
    assert_eq!(stash_display_title("   ", "stash@{1}"), "stash@{1}");
}

#[test]
fn format_relative_time_from_uses_compact_human_units() {
    let now = 1_800_000_000;
    assert_eq!(format_relative_time_from(now, now), "now");
    assert_eq!(format_relative_time_from(now, now - 90), "1m");
    assert_eq!(format_relative_time_from(now, now - 3_600), "1h");
    assert_eq!(format_relative_time_from(now, now - 86_400 * 8), "8d");
    assert_eq!(format_relative_time_from(now, now - 86_400 * 45), "1mo");
}

#[test]
fn build_stash_line_cache_filters_file_headers_and_tracks_width() {
    let document = stash_file_document(
        "src/lib.rs",
        vec![
            diff_line(
                "diff --git a/src/lib.rs b/src/lib.rs",
                DiffLineKind::FileHeader,
            ),
            diff_line("short", DiffLineKind::Context),
            diff_line("the longest stash line", DiffLineKind::Addition),
        ],
    );

    let cache = build_stash_line_cache(&document, ThemeId::Dark);

    assert_eq!(cache.selection, document.selection);
    assert_eq!(cache.theme_id, ThemeId::Dark);
    assert_eq!(cache.lines.len(), 2);
    assert!(cache
        .lines
        .iter()
        .all(|line| line.kind != DiffLineKind::FileHeader));
    assert_eq!(cache.max_line_chars, "the longest stash line".len());
    assert!(!cache.is_oversize);
}

#[test]
fn build_stash_line_cache_marks_oversize_notice() {
    let document = stash_file_document(
        "Cargo.lock",
        vec![diff_line(OVERSIZE_DIFF_MESSAGE, DiffLineKind::BinaryNotice)],
    );

    let cache = build_stash_line_cache(&document, ThemeId::Dark);

    assert!(cache.is_oversize);
    assert_eq!(cache.lines.len(), 1);
}

#[test]
fn stash_line_cache_key_requires_stash_mode_and_selection() {
    let selection = stash_selection("src/lib.rs");

    assert!(stash_line_cache_key(DiffTabViewMode::WorkingTree, Some(&selection)).is_none());
    assert!(stash_line_cache_key(DiffTabViewMode::Stashes, None).is_none());
    assert_eq!(
        stash_line_cache_key(DiffTabViewMode::Stashes, Some(&selection)),
        Some(selection)
    );
}

#[test]
fn stash_line_cache_matches_requires_same_selection_and_theme() {
    let cached = stash_selection("src/lib.rs");
    let same = stash_selection("src/lib.rs");
    let different = stash_selection("src/main.rs");

    assert!(stash_line_cache_matches(
        &cached,
        ThemeId::Dark,
        &same,
        ThemeId::Dark
    ));
    assert!(!stash_line_cache_matches(
        &cached,
        ThemeId::Dark,
        &different,
        ThemeId::Dark
    ));
    assert!(!stash_line_cache_matches(
        &cached,
        ThemeId::Dark,
        &same,
        ThemeId::Light
    ));
}

#[test]
fn plain_text_for_line_joins_spans() {
    use orcashell_git::HighlightedSpan;
    let spans = vec![
        HighlightedSpan {
            text: "fn ".to_string(),
            color: 0xFF0000,
        },
        HighlightedSpan {
            text: "main".to_string(),
            color: 0x00FF00,
        },
    ];
    assert_eq!(plain_text_for_line("fn main", Some(&spans)), "fn main");
}

#[test]
fn plain_text_for_line_normalizes_whitespace() {
    assert_eq!(plain_text_for_line("a\tb\n", None), "a    b");
}

#[test]
fn selection_range_single_line() {
    let line = diff_line("hello world", DiffLineKind::Addition);
    let sel = DiffSelection {
        start: (0, 0),
        end: (0, 5),
        is_selecting: false,
    };
    let range = selection_range_for_line(Some(&sel), 0, &line);
    assert!(range.is_some());
}

#[test]
fn selection_range_outside_line() {
    let line = diff_line("hello", DiffLineKind::Context);
    let sel = DiffSelection {
        start: (2, 0),
        end: (3, 5),
        is_selecting: false,
    };
    assert!(selection_range_for_line(Some(&sel), 0, &line).is_none());
}

#[test]
fn extract_selected_text_single_line() {
    let line = diff_line("hello world", DiffLineKind::Addition);
    let lines: Vec<&DiffLineView> = vec![&line];
    let text = extract_selected_text(&lines, 0, 6, 0, 11);
    assert_eq!(text, Some("world".to_string()));
}

#[test]
fn extract_selected_text_multi_line() {
    let l0 = diff_line("first line", DiffLineKind::Context);
    let l1 = diff_line("second line", DiffLineKind::Addition);
    let l2 = diff_line("third line", DiffLineKind::Context);
    let lines: Vec<&DiffLineView> = vec![&l0, &l1, &l2];
    let text = extract_selected_text(&lines, 0, 6, 2, 5);
    assert_eq!(text, Some("line\nsecond line\nthird".to_string()));
}

#[test]
fn extract_selected_text_normalizes_nbsp_in_highlighted_spans() {
    use orcashell_git::HighlightedSpan;
    let line = DiffLineView {
        kind: DiffLineKind::Context,
        old_lineno: Some(1),
        new_lineno: Some(1),
        text: "fn main() {".to_string(),
        highlights: Some(vec![
            HighlightedSpan {
                text: "fn\u{00A0}".to_string(),
                color: 0xFF0000,
            },
            HighlightedSpan {
                text: "main()\u{00A0}{".to_string(),
                color: 0x00FF00,
            },
        ]),
        inline_changes: None,
    };
    let lines: Vec<&DiffLineView> = vec![&line];
    let text = extract_selected_text(&lines, 0, 0, 0, 11);
    // Clipboard text must have regular spaces, not NBSPs.
    assert_eq!(text, Some("fn main() {".to_string()));
}

#[test]
fn collect_file_keys_from_tree() {
    let tree = build_diff_tree(&[
        file("src/lib.rs", GitFileStatus::Modified),
        file("src/app/mod.rs", GitFileStatus::Added),
        file("README.md", GitFileStatus::Modified),
    ]);
    let mut keys = Vec::new();
    collect_file_keys(&tree, DiffSectionKind::Unstaged, &mut keys);
    assert_eq!(keys.len(), 3);
    assert_eq!(
        keys[0],
        DiffSelectionKey {
            section: DiffSectionKind::Unstaged,
            relative_path: PathBuf::from("src/app/mod.rs"),
        }
    );
    assert_eq!(
        keys[1],
        DiffSelectionKey {
            section: DiffSectionKind::Unstaged,
            relative_path: PathBuf::from("src/lib.rs"),
        }
    );
    assert_eq!(
        keys[2],
        DiffSelectionKey {
            section: DiffSectionKind::Unstaged,
            relative_path: PathBuf::from("README.md"),
        }
    );
}

#[test]
fn line_cache_match_requires_section_aware_selection() {
    let staged = DiffSelectionKey {
        section: DiffSectionKind::Staged,
        relative_path: PathBuf::from("src/lib.rs"),
    };
    let unstaged = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("src/lib.rs"),
    };

    assert!(line_cache_matches(&staged, 7, &staged, 7));
    assert!(!line_cache_matches(&staged, 7, &unstaged, 7));
}

#[test]
fn copy_keystroke_matches_platform_and_control_c() {
    assert!(is_copy_keystroke(
        "c",
        &Modifiers {
            platform: true,
            ..Default::default()
        }
    ));
    assert!(is_copy_keystroke(
        "c",
        &Modifiers {
            control: true,
            ..Default::default()
        }
    ));
    assert!(!is_copy_keystroke("c", &Modifiers::default()));
    assert!(!is_copy_keystroke(
        "x",
        &Modifiers {
            platform: true,
            ..Default::default()
        }
    ));
}

#[test]
fn conflict_render_lines_classify_diff3_blocks() {
    let lines = build_conflict_render_lines(
        "<<<<<<< ours\nleft\n||||||| base\ncommon\n=======\nright\n>>>>>>> theirs\n",
        PathBuf::from("conflict.rs").as_path(),
        ThemeId::Dark,
    );

    assert_eq!(lines[0].kind, DiffLineKind::ConflictMarker);
    assert_eq!(lines[1].kind, DiffLineKind::ConflictOurs);
    assert_eq!(lines[2].kind, DiffLineKind::ConflictMarker);
    assert_eq!(lines[3].kind, DiffLineKind::ConflictBase);
    assert_eq!(lines[4].kind, DiffLineKind::ConflictMarker);
    assert_eq!(lines[5].kind, DiffLineKind::ConflictTheirs);
    assert_eq!(lines[6].kind, DiffLineKind::ConflictMarker);
    assert!(lines.iter().take(7).all(|line| line.block_index == Some(0)));
}

#[test]
fn conflict_render_lines_include_syntax_highlights_for_code() {
    let lines = build_conflict_render_lines(
            "fn example() {\n<<<<<<< ours\n    let value = 1;\n=======\n    let value = 2;\n>>>>>>> theirs\n}\n",
            PathBuf::from("conflict.rs").as_path(),
            ThemeId::Dark,
        );

    assert!(lines[0].highlights.is_some());
    assert!(lines[2].highlights.is_some());
    assert!(lines[3].highlights.is_none());
    assert!(lines[4].highlights.is_some());
    assert!(lines[6].highlights.is_some());
}

#[test]
fn conflict_render_lines_preserve_multiline_state_inside_branches() {
    let lines = build_conflict_render_lines(
            "fn example() {\n    /* start comment\n<<<<<<< ours\ncomment alpha\n=======\ncomment beta\n>>>>>>> theirs\n    end comment */\n}\n",
            PathBuf::from("conflict.rs").as_path(),
            ThemeId::Dark,
        );

    assert_eq!(
        lines[3].highlight_mode,
        super::ConflictHighlightMode::Stateful
    );
    assert_eq!(
        lines[5].highlight_mode,
        super::ConflictHighlightMode::Stateful
    );
    for line in [&lines[3], &lines[5]] {
        let spans = line
            .highlights
            .as_ref()
            .expect("branch lines should be highlighted");
        for span in spans.iter().filter(|span| !span.text.trim().is_empty()) {
            assert_eq!(span.color, 0x9499A8);
        }
    }
    assert_eq!(
        lines[7].highlight_mode,
        super::ConflictHighlightMode::Stateful
    );
}

#[test]
fn conflict_render_lines_fall_back_after_ambiguous_branch_exit() {
    let lines = build_conflict_render_lines(
            "fn example() {\n<<<<<<< ours\n    let value = r#\"\n=======\n    let value = 1;\n>>>>>>> theirs\nstill ambiguous\n}\n",
            PathBuf::from("conflict.rs").as_path(),
            ThemeId::Dark,
        );

    assert_eq!(lines[6].kind, DiffLineKind::Context);
    assert_eq!(
        lines[6].highlight_mode,
        super::ConflictHighlightMode::Fallback
    );
    assert!(lines[6].highlights.is_some());
}

#[test]
fn conflict_render_lines_incremental_rebuild_matches_full_rebuild() {
    let original = "fn example() {\n    let value = r#\"\n<<<<<<< ours\nours text\n=======\ntheirs text\n>>>>>>> theirs\n\"#;\n}\n";
    let edited = "fn example() {\n    let value = r#\"\n<<<<<<< ours\nours updated text\n=======\ntheirs text\n>>>>>>> theirs\n\"#;\n}\n";
    let prior_cache = cache_for_conflict_text(original, "conflict.rs");

    let (incremental_lines, _) = build_conflict_render_lines_with_cache(
        edited,
        PathBuf::from("conflict.rs").as_path(),
        ThemeId::Dark,
        Some(&prior_cache),
    );
    let (full_lines, _) = build_conflict_render_lines_with_cache(
        edited,
        PathBuf::from("conflict.rs").as_path(),
        ThemeId::Dark,
        None,
    );

    assert_eq!(
        conflict_line_fingerprint(&incremental_lines),
        conflict_line_fingerprint(&full_lines)
    );
}

#[test]
fn conflict_navigation_from_no_active_block_targets_expected_edge() {
    assert_eq!(navigated_conflict_block_index(3, None, true), Some(0));
    assert_eq!(navigated_conflict_block_index(3, None, false), Some(2));
    assert_eq!(navigated_conflict_block_index(1, None, true), Some(0));
    assert_eq!(navigated_conflict_block_index(1, None, false), Some(0));
}

#[test]
fn conflict_navigation_buttons_stay_enabled_without_active_block() {
    assert!(!conflict_block_navigation_disabled(3, None, true));
    assert!(!conflict_block_navigation_disabled(3, None, false));
    assert!(!conflict_block_navigation_disabled(1, None, true));
    assert!(!conflict_block_navigation_disabled(1, None, false));
    assert!(conflict_block_navigation_disabled(0, None, true));
    assert!(conflict_block_navigation_disabled(0, None, false));
}

#[test]
fn replace_document_range_reparses_conflict_blocks() {
    let parsed =
        parse_conflict_file_text("<<<<<<< ours\nleft\n=======\nright\n>>>>>>> theirs\n").unwrap();
    let mut document = ConflictEditorDocument {
        generation: 1,
        version: 0,
        selection: DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("conflict.rs"),
        },
        file: file("conflict.rs", GitFileStatus::Conflicted),
        initial_raw_text: parsed.raw_text.clone(),
        raw_text: parsed.raw_text,
        blocks: parsed.blocks,
        has_base_sections: false,
        parse_error: None,
        is_dirty: false,
        cursor_pos: 0,
        selection_range: None,
        active_block_index: Some(0),
        scroll_x: 0.0,
        scroll_y: 0.0,
    };

    let block = document.blocks[0].clone();
    replace_document_range(&mut document, block.whole_block, "left\nright\n");

    assert!(document.is_dirty);
    assert!(document.blocks.is_empty());
    assert!(document.parse_error.is_none());
    assert_eq!(document.raw_text, "left\nright\n");
}

#[test]
fn delete_document_backward_removes_previous_character() {
    let mut document = ConflictEditorDocument {
        generation: 1,
        version: 0,
        selection: DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("conflict.rs"),
        },
        file: file("conflict.rs", GitFileStatus::Conflicted),
        initial_raw_text: "abc".to_string(),
        raw_text: "abc".to_string(),
        blocks: Vec::new(),
        has_base_sections: false,
        parse_error: None,
        is_dirty: false,
        cursor_pos: 2,
        selection_range: None,
        active_block_index: None,
        scroll_x: 0.0,
        scroll_y: 0.0,
    };

    assert!(delete_document_backward(&mut document));
    assert_eq!(document.raw_text, "ac");
    assert_eq!(document.cursor_pos, 1);
    assert!(document.selection_range.is_none());
    assert!(document.is_dirty);
}

#[test]
fn delete_document_forward_removes_next_character() {
    let mut document = ConflictEditorDocument {
        generation: 1,
        version: 0,
        selection: DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("conflict.rs"),
        },
        file: file("conflict.rs", GitFileStatus::Conflicted),
        initial_raw_text: "abc".to_string(),
        raw_text: "abc".to_string(),
        blocks: Vec::new(),
        has_base_sections: false,
        parse_error: None,
        is_dirty: false,
        cursor_pos: 1,
        selection_range: None,
        active_block_index: None,
        scroll_x: 0.0,
        scroll_y: 0.0,
    };

    assert!(delete_document_forward(&mut document));
    assert_eq!(document.raw_text, "ac");
    assert_eq!(document.cursor_pos, 1);
    assert!(document.selection_range.is_none());
    assert!(document.is_dirty);
}

#[test]
fn indent_document_selection_inserts_spaces_at_cursor_without_selection() {
    let mut document = ConflictEditorDocument {
        generation: 1,
        version: 0,
        selection: DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("conflict.rs"),
        },
        file: file("conflict.rs", GitFileStatus::Conflicted),
        initial_raw_text: "fn main() {}\n".to_string(),
        raw_text: "fn main() {}\n".to_string(),
        blocks: Vec::new(),
        has_base_sections: false,
        parse_error: None,
        is_dirty: false,
        cursor_pos: 3,
        selection_range: None,
        active_block_index: None,
        scroll_x: 0.0,
        scroll_y: 0.0,
    };

    assert!(indent_document_selection(&mut document));
    assert_eq!(document.raw_text, "fn     main() {}\n");
    assert_eq!(document.cursor_pos, 7);
    assert!(document.selection_range.is_none());
}

#[test]
fn indent_document_selection_multiline_indents_all_selected_lines() {
    let text = "alpha\nbeta\ngamma\n";
    let mut document = ConflictEditorDocument {
        generation: 1,
        version: 0,
        selection: DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("conflict.rs"),
        },
        file: file("conflict.rs", GitFileStatus::Conflicted),
        initial_raw_text: text.to_string(),
        raw_text: text.to_string(),
        blocks: Vec::new(),
        has_base_sections: false,
        parse_error: None,
        is_dirty: false,
        cursor_pos: 10,
        selection_range: Some(1..10),
        active_block_index: None,
        scroll_x: 0.0,
        scroll_y: 0.0,
    };

    assert!(indent_document_selection(&mut document));
    assert_eq!(document.raw_text, "    alpha\n    beta\ngamma\n");
    assert_eq!(document.selection_range, Some(5..18));
    assert_eq!(document.cursor_pos, 18);
}

#[test]
fn outdent_document_selection_removes_spaces_from_current_line() {
    let mut document = ConflictEditorDocument {
        generation: 1,
        version: 0,
        selection: DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("conflict.rs"),
        },
        file: file("conflict.rs", GitFileStatus::Conflicted),
        initial_raw_text: "    fn main() {}\n".to_string(),
        raw_text: "    fn main() {}\n".to_string(),
        blocks: Vec::new(),
        has_base_sections: false,
        parse_error: None,
        is_dirty: false,
        cursor_pos: 8,
        selection_range: None,
        active_block_index: None,
        scroll_x: 0.0,
        scroll_y: 0.0,
    };

    assert!(outdent_document_selection(&mut document));
    assert_eq!(document.raw_text, "fn main() {}\n");
    assert_eq!(document.cursor_pos, 4);
    assert!(document.selection_range.is_none());
}

#[test]
fn outdent_document_selection_multiline_updates_selection() {
    let text = "    alpha\n    beta\ngamma\n";
    let mut document = ConflictEditorDocument {
        generation: 1,
        version: 0,
        selection: DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("conflict.rs"),
        },
        file: file("conflict.rs", GitFileStatus::Conflicted),
        initial_raw_text: text.to_string(),
        raw_text: text.to_string(),
        blocks: Vec::new(),
        has_base_sections: false,
        parse_error: None,
        is_dirty: false,
        cursor_pos: 18,
        selection_range: Some(5..18),
        active_block_index: None,
        scroll_x: 0.0,
        scroll_y: 0.0,
    };

    assert!(outdent_document_selection(&mut document));
    assert_eq!(document.raw_text, "alpha\nbeta\ngamma\n");
    assert_eq!(document.selection_range, Some(1..10));
    assert_eq!(document.cursor_pos, 10);
}

#[test]
fn diff_line_colors_include_conflict_variants() {
    let palette = crate::theme::OrcaTheme::dark();

    let marker = diff_line_colors(&palette, DiffLineKind::ConflictMarker);
    let ours = diff_line_colors(&palette, DiffLineKind::ConflictOurs);
    let base = diff_line_colors(&palette, DiffLineKind::ConflictBase);
    let theirs = diff_line_colors(&palette, DiffLineKind::ConflictTheirs);

    assert_eq!(marker.1, palette.STATUS_AMBER);
    assert_eq!(ours.1, palette.BONE);
    assert_eq!(base.1, palette.BONE);
    assert_eq!(theirs.1, palette.BONE);
    assert!(marker.0.is_some());
    assert!(ours.0.is_some());
    assert!(base.0.is_some());
    assert!(theirs.0.is_some());
}
