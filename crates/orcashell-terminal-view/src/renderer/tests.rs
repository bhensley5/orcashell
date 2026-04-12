use super::*;

#[test]
fn test_renderer_creation() {
    let renderer = TerminalRenderer::new(
        "Fira Code".to_string(),
        px(14.0),
        1.0,
        ColorPalette::default(),
    );
    assert_eq!(renderer.font_family, "Fira Code");
    assert_eq!(renderer.font_size, px(14.0));
    assert_eq!(renderer.line_height_multiplier, 1.0);
}

#[test]
fn test_background_rect_merge() {
    let black = Hsla::black();

    let rect1 = BackgroundRect {
        start_col: 0,
        end_col: 5,
        row: 0,
        color: black,
    };

    let rect2 = BackgroundRect {
        start_col: 5,
        end_col: 10,
        row: 0,
        color: black,
    };

    assert!(rect1.can_merge_with(&rect2));

    let rect3 = BackgroundRect {
        start_col: 5,
        end_col: 10,
        row: 1,
        color: black,
    };

    assert!(!rect1.can_merge_with(&rect3));
}

#[test]
fn test_selection_highlight_coordinate_conversion() {
    use alacritty_terminal::selection::SelectionRange;

    // Simulate: selection from buffer line 0, col 2 to buffer line 1, col 5
    let range = SelectionRange::new(
        AlacPoint::new(Line(0), Column(2)),
        AlacPoint::new(Line(1), Column(5)),
        false,
    );

    // display_offset = 0: visual line matches buffer line
    let display_offset: i32 = 0;
    let visual_line: usize = 0;
    let col = Column(3);
    let buffer_point = AlacPoint::new(Line(visual_line as i32 - display_offset), col);
    assert!(range.contains(buffer_point));

    // Visual line 1, col 6 → buffer line 1, col 6 → outside selection end (col 5)
    let buffer_point = AlacPoint::new(Line(1 - display_offset), Column(6));
    assert!(!range.contains(buffer_point));

    // display_offset = 3: user scrolled up 3 lines into history
    // Visual line 0 → buffer line -3 (in scrollback)
    let display_offset: i32 = 3;
    let visual_line: usize = 0;
    let buffer_point = AlacPoint::new(Line(visual_line as i32 - display_offset), Column(3));
    // buffer line -3 is above selection start (line 0), so not selected
    assert!(!range.contains(buffer_point));

    // Visual line 3 → buffer line 0 → in selection
    let visual_line: usize = 3;
    let buffer_point = AlacPoint::new(Line(visual_line as i32 - display_offset), Column(3));
    assert!(range.contains(buffer_point));
}

#[test]
fn test_selection_span_analytical() {
    let num_cols = 80;

    // Multi-line selection: line 0 col 5 → line 2 col 10
    let range = SelectionRange::new(
        AlacPoint::new(Line(0), Column(5)),
        AlacPoint::new(Line(2), Column(10)),
        false,
    );

    // Row above selection → None
    assert_eq!(
        selection_span_for_row(&range, 0, 1, num_cols), // visual 0, offset 1 → buffer -1
        None
    );

    // First line of selection (display_offset=0)
    assert_eq!(
        selection_span_for_row(&range, 0, 0, num_cols),
        Some((5, 80))
    );

    // Middle line → full row
    assert_eq!(
        selection_span_for_row(&range, 1, 0, num_cols),
        Some((0, 80))
    );

    // Last line → 0..end+1
    assert_eq!(
        selection_span_for_row(&range, 2, 0, num_cols),
        Some((0, 11))
    );

    // Row below selection → None
    assert_eq!(selection_span_for_row(&range, 3, 0, num_cols), None);

    // Single-line selection: line 1 col 3 → line 1 col 7
    let range_single = SelectionRange::new(
        AlacPoint::new(Line(1), Column(3)),
        AlacPoint::new(Line(1), Column(7)),
        false,
    );
    assert_eq!(
        selection_span_for_row(&range_single, 1, 0, num_cols),
        Some((3, 8))
    );

    // Block selection: same column range on every line
    let range_block = SelectionRange::new(
        AlacPoint::new(Line(0), Column(5)),
        AlacPoint::new(Line(2), Column(10)),
        true,
    );
    assert_eq!(
        selection_span_for_row(&range_block, 0, 0, num_cols),
        Some((5, 11))
    );
    assert_eq!(
        selection_span_for_row(&range_block, 1, 0, num_cols),
        Some((5, 11))
    );
    assert_eq!(
        selection_span_for_row(&range_block, 2, 0, num_cols),
        Some((5, 11))
    );

    // With display_offset: visual line 3, offset 3 → buffer line 0
    assert_eq!(
        selection_span_for_row(&range, 3, 3, num_cols),
        Some((5, 80))
    );
}

#[test]
fn test_selection_span_matches_brute_force() {
    // Verify analytical results match the O(cols) brute-force approach
    let num_cols = 20;
    let range = SelectionRange::new(
        AlacPoint::new(Line(1), Column(5)),
        AlacPoint::new(Line(3), Column(10)),
        false,
    );
    let display_offset: i32 = 0;

    for visual_line in 0..6 {
        // Brute-force: scan all columns
        let mut bf_start: Option<usize> = None;
        let mut bf_end: usize = 0;
        for col_idx in 0..num_cols {
            let buffer_point =
                AlacPoint::new(Line(visual_line as i32 - display_offset), Column(col_idx));
            if range.contains(buffer_point) {
                if bf_start.is_none() {
                    bf_start = Some(col_idx);
                }
                bf_end = col_idx + 1;
            }
        }
        let brute_force = bf_start.map(|s| (s, bf_end));

        let analytical = selection_span_for_row(&range, visual_line, display_offset, num_cols);

        assert_eq!(
            analytical, brute_force,
            "Mismatch at visual_line={visual_line}"
        );
    }
}

#[test]
fn test_search_span_matches_brute_force() {
    use crate::search::VisibleMatch;
    use std::ops::RangeInclusive;

    let num_cols = 20;
    let display_offset: i32 = 0;

    // Multi-line match: buffer line 1 col 3 → line 2 col 8
    let match_range: RangeInclusive<AlacPoint> =
        AlacPoint::new(Line(1), Column(3))..=AlacPoint::new(Line(2), Column(8));
    let vm = VisibleMatch {
        range: match_range,
        is_current: false,
    };

    for visual_line in 0..5 {
        // Brute-force
        let mut bf_start: Option<usize> = None;
        let mut bf_end: usize = 0;
        for col_idx in 0..num_cols {
            let buffer_point =
                AlacPoint::new(Line(visual_line as i32 - display_offset), Column(col_idx));
            let in_range = buffer_point >= *vm.range.start() && buffer_point <= *vm.range.end();
            if in_range {
                if bf_start.is_none() {
                    bf_start = Some(col_idx);
                }
                bf_end = col_idx + 1;
            }
        }
        let brute_force = bf_start.map(|s| (s, bf_end));
        let analytical = search_span_for_row(&vm, visual_line, display_offset, num_cols);

        assert_eq!(
            analytical, brute_force,
            "Search span mismatch at visual_line={visual_line}"
        );
    }
}

#[test]
fn test_hovered_link_span_matches_brute_force() {
    use crate::links::VisibleHoveredLink;
    use std::ops::RangeInclusive;

    let num_cols = 20;
    let display_offset: i32 = 0;

    let link_range: RangeInclusive<AlacPoint> =
        AlacPoint::new(Line(1), Column(4))..=AlacPoint::new(Line(2), Column(6));
    let hovered = VisibleHoveredLink {
        range: link_range,
        underline_color: None,
    };

    for visual_line in 0..5 {
        let mut bf_start: Option<usize> = None;
        let mut bf_end: usize = 0;
        for col_idx in 0..num_cols {
            let buffer_point =
                AlacPoint::new(Line(visual_line as i32 - display_offset), Column(col_idx));
            let in_range =
                buffer_point >= *hovered.range.start() && buffer_point <= *hovered.range.end();
            if in_range {
                if bf_start.is_none() {
                    bf_start = Some(col_idx);
                }
                bf_end = col_idx + 1;
            }
        }
        let brute_force = bf_start.map(|s| (s, bf_end));
        let analytical = hovered_link_span_for_row(&hovered, visual_line, display_offset, num_cols);

        assert_eq!(
            analytical, brute_force,
            "Hovered link span mismatch at visual_line={visual_line}"
        );
    }
}

#[test]
fn test_cache_invalidation() {
    let mut cache = LineCache::new();

    // First validate populates cache
    assert!(!cache.validate(24, 80, 0, px(14.0), 0));
    assert_eq!(cache.rows.len(), 24);

    // Same params → valid
    assert!(cache.validate(24, 80, 0, px(14.0), 0));

    // Scroll change → invalidate
    assert!(!cache.validate(24, 80, 1, px(14.0), 0));

    // Font size change → invalidate
    assert!(cache.validate(24, 80, 1, px(14.0), 0));
    assert!(!cache.validate(24, 80, 1, px(16.0), 0));

    // Palette generation change → invalidate
    assert!(cache.validate(24, 80, 1, px(16.0), 0));
    assert!(!cache.validate(24, 80, 1, px(16.0), 1));

    // Resize → invalidate
    assert!(cache.validate(24, 80, 1, px(16.0), 1));
    assert!(!cache.validate(30, 80, 1, px(16.0), 1));
}
