//! Terminal rendering module.
//!
//! This module provides [`TerminalRenderer`], which handles efficient rendering of
//! terminal content using GPUI's text and drawing systems.
//!
//! # Rendering Pipeline
//!
//! The renderer processes the terminal grid in several stages:
//!
//! ```text
//! Terminal Grid → Layout Phase → Paint Phase
//!                      │              │
//!                      ├─ Collect backgrounds
//!                      ├─ Batch text runs
//!                      │              │
//!                      │              ├─ Paint default background
//!                      │              ├─ Paint non-default backgrounds
//!                      │              ├─ Paint text characters
//!                      │              └─ Paint cursor
//! ```
//!
//! # Optimizations
//!
//! The renderer includes several optimizations to minimize draw calls:
//!
//! 1. **Background Merging**: Adjacent cells with the same background color are
//!    merged into single rectangles, reducing the number of quads to paint.
//!
//! 2. **Text Batching**: Adjacent cells with identical styling (color, bold, italic)
//!    are grouped into [`BatchedTextRun`]s for efficient text shaping.
//!
//! 3. **Default Background Skip**: Cells with the default background color don't
//!    generate separate background rectangles.
//!
//! 4. **Cell Measurement**: Font metrics are measured once using the '│' (BOX DRAWINGS
//!    LIGHT VERTICAL) character and cached for consistent cell dimensions.
//!
//! # Cell Dimensions
//!
//! Cell size is calculated from actual font metrics using the '│' character,
//! which spans the full cell height in properly designed terminal fonts:
//!
//! - **Width**: Measured from shaped '│' character
//! - **Height**: `(ascent + descent) × line_height_multiplier`
//!
//! The `line_height_multiplier` (default 1.0) can be adjusted to add extra
//! vertical space if needed for specific fonts.
//!
//! # Example
//!
//! ```ignore
//! use gpui::px;
//! use orcashell_terminal_view::{ColorPalette, TerminalRenderer};
//!
//! let renderer = TerminalRenderer::new(
//!     "JetBrains Mono".to_string(),
//!     px(14.0),
//!     1.0,  // line height multiplier
//!     ColorPalette::default(),
//! );
//! ```

use crate::box_drawing;
use crate::colors::ColorPalette;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point as AlacPoint};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::Color;
use gpui::{
    px, quad, transparent_black, App, Bounds, Edges, Font, FontFeatures, FontStyle, FontWeight,
    Hsla, Pixels, Point, ShapedLine, SharedString, Size, StrikethroughStyle, TextRun,
    UnderlineStyle, Window,
};
use std::cell::RefCell;
use std::rc::Rc;

/// Underline style variant detected from cell flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnderlineVariant {
    None,
    Single,
    Double,
    Wavy,
    Dotted,
    Dashed,
}

/// A batched run of text with consistent styling.
///
/// This struct groups adjacent terminal cells with identical visual attributes
/// to reduce the number of text rendering calls.
#[derive(Debug, Clone)]
pub struct BatchedTextRun {
    /// The text content to render
    pub text: String,

    /// Starting column position
    pub start_col: usize,

    /// Row position
    pub row: usize,

    /// Foreground color
    pub fg_color: Hsla,

    /// Background color
    pub bg_color: Hsla,

    /// Bold flag
    pub bold: bool,

    /// Italic flag
    pub italic: bool,

    /// Underline style variant
    pub underline_variant: UnderlineVariant,

    /// Underline color (None = use fg_color)
    pub underline_color: Option<Hsla>,

    /// Whether this cell is part of an OSC 8 hyperlink
    pub has_hyperlink: bool,

    /// Strikethrough flag
    pub strikethrough: bool,
}

/// Background rectangle to paint.
///
/// Represents a rectangular region with a solid color background.
#[derive(Debug, Clone)]
pub struct BackgroundRect {
    /// Starting column position
    pub start_col: usize,

    /// Ending column position (exclusive)
    pub end_col: usize,

    /// Row position
    pub row: usize,

    /// Background color
    pub color: Hsla,
}

impl BackgroundRect {
    /// Check if this rectangle can be merged with another.
    ///
    /// Two rectangles can be merged if they:
    /// - Are on the same row
    /// - Have the same color
    /// - Are horizontally adjacent
    #[cfg(test)]
    fn can_merge_with(&self, other: &Self) -> bool {
        self.row == other.row && self.color == other.color && self.end_col == other.start_col
    }
}

/// A pre-shaped text run with cached position offsets for paint replay.
/// Offsets are relative to the content origin so they remain valid across
/// window moves; absolute position is computed at paint time.
#[derive(Clone)]
struct CachedShapedRun {
    shaped_line: ShapedLine,
    /// X offset from content origin (column-based, unrounded).
    offset_x: Pixels,
    /// Y offset from content origin (row-based with vertical centering, unrounded).
    offset_y: Pixels,
}

/// Cached box-drawing paint command for replay.
#[derive(Clone)]
enum CachedBoxDraw {
    /// Horizontal span across multiple cells with the same weight and color.
    HorizontalSpan {
        start_col: usize,
        end_col_inclusive: usize,
        weight: box_drawing::LineWeight,
        color: Hsla,
    },
    /// Just the vertical components of a box char (horizontal already drawn by span).
    VerticalComponents { col: usize, ch: char, color: Hsla },
    /// Full box character (not part of a horizontal span).
    FullChar { col: usize, ch: char, color: Hsla },
    /// Block element (U+2580-U+259F). Rendered as filled rects to avoid font glyph gaps.
    BlockElement { col: usize, ch: char, color: Hsla },
}

/// Cached custom underline paint command for replay.
/// Used for underline styles that GPUI cannot render natively (double, dotted, dashed).
#[derive(Clone)]
enum CachedUnderline {
    Double {
        start_col: usize,
        end_col: usize,
        color: Hsla,
    },
    Dotted {
        start_col: usize,
        end_col: usize,
        color: Hsla,
    },
    Dashed {
        start_col: usize,
        end_col: usize,
        color: Hsla,
    },
}

/// Cached layout data for a single terminal row.
/// Stores paint-ready data so undamaged rows skip cell collection,
/// layout computation, and text shaping entirely.
#[derive(Clone)]
struct CachedRowLayout {
    backgrounds: Vec<BackgroundRect>,
    shaped_runs: Vec<CachedShapedRun>,
    box_draws: Vec<CachedBoxDraw>,
    underlines: Vec<CachedUnderline>,
}

/// Per-frame line cache. Stores layout results for each visible row so that
/// undamaged rows skip cell collection, color resolution, and layout computation.
pub struct LineCache {
    rows: Vec<Option<CachedRowLayout>>,
    /// Display offset when cache was last populated. Scrolling invalidates everything.
    display_offset: i32,
    /// Grid dimensions when cache was last populated. Resize invalidates everything.
    num_cols: usize,
    num_lines: usize,
    /// Font size when cache was last populated. Font change invalidates ShapedLine data.
    font_size: Pixels,
    /// Palette generation when cache was last populated. Color change invalidates ShapedLine data.
    palette_generation: u64,
}

impl Default for LineCache {
    fn default() -> Self {
        Self::new()
    }
}

impl LineCache {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            display_offset: 0,
            num_cols: 0,
            num_lines: 0,
            font_size: px(0.0),
            palette_generation: 0,
        }
    }

    /// Check if the cache geometry matches current terminal state.
    /// If not, invalidate everything. Checks dimensions, scroll position,
    /// font size, and palette generation.
    fn validate(
        &mut self,
        num_lines: usize,
        num_cols: usize,
        display_offset: i32,
        font_size: Pixels,
        palette_generation: u64,
    ) -> bool {
        if self.num_lines != num_lines
            || self.num_cols != num_cols
            || self.display_offset != display_offset
            || self.font_size != font_size
            || self.palette_generation != palette_generation
        {
            self.rows.clear();
            self.rows.resize(num_lines, None);
            self.num_lines = num_lines;
            self.num_cols = num_cols;
            self.display_offset = display_offset;
            self.font_size = font_size;
            self.palette_generation = palette_generation;
            false
        } else {
            true
        }
    }
}

#[derive(Clone, Copy)]
struct RowCellRange {
    start: usize,
    end: usize,
}

/// Terminal state captured under lock for lock-free rendering.
///
/// `snapshot_frame` populates this struct while the terminal lock is held,
/// then the caller drops the lock and passes the snapshot to
/// `paint_from_snapshot` which performs layout, text shaping, and painting
/// without needing the lock.
pub(crate) struct FrameSnapshot {
    num_lines: usize,
    num_cols: usize,
    display_offset: i32,
    /// Flat cell storage for all rows that need recomputation.
    cells: Vec<(usize, Cell)>,
    /// Ranges into `cells`, indexed by line_idx.
    /// `Some(range)` = row needs layout/shaping (damaged, cache-invalid, or uncached).
    /// `None` = row is undamaged and cached, skip recomputation.
    row_cell_ranges: Vec<Option<RowCellRange>>,
    colors: Colors,
    selection_range: Option<SelectionRange>,
    cursor_point: AlacPoint,
    term_mode: TermMode,
    history_size: usize,
}

impl FrameSnapshot {
    pub(crate) fn num_lines(&self) -> usize {
        self.num_lines
    }

    pub(crate) fn display_offset(&self) -> usize {
        self.display_offset.max(0) as usize
    }

    fn row_cells(&self, line_idx: usize) -> Option<&[(usize, Cell)]> {
        let range = self
            .row_cell_ranges
            .get(line_idx)
            .and_then(|range| *range)?;
        Some(&self.cells[range.start..range.end])
    }
}

/// Terminal renderer with font settings and cell dimensions.
///
/// This struct manages the rendering of terminal content, including text,
/// backgrounds, and cursor. It maintains font metrics and provides the
/// [`paint`](Self::paint) method for drawing the terminal grid.
///
/// # Font Metrics
///
/// Cell dimensions are calculated from actual font measurements via
/// [`measure_cell`](Self::measure_cell). This ensures accurate character
/// positioning regardless of the font used.
///
/// # Usage
///
/// The renderer is typically used internally by [`TerminalView`](crate::TerminalView),
/// but can also be used directly for custom rendering:
///
/// ```ignore
/// // Measure cell dimensions (call once per font change)
/// renderer.measure_cell(window);
///
/// // Snapshot terminal state under lock, then paint without the lock
/// let snapshot = renderer.snapshot_frame(&mut term);
/// drop(term);
/// renderer.paint_from_snapshot(bounds, padding, &snapshot, cursor_visible, cursor_shape, &matches, window, cx);
/// ```
///
/// # Performance
///
/// For optimal performance:
/// - Call `measure_cell` only when font settings change
/// - The `paint` method is designed to be called every frame
/// - Background and text batching minimize GPU draw calls
#[derive(Clone)]
pub struct TerminalRenderer {
    /// Font family name (e.g., "Fira Code", "Menlo")
    pub font_family: String,

    /// Cached SharedString for font family. Avoids per-cell allocation
    pub(crate) font_family_shared: SharedString,

    /// Font size in pixels
    pub font_size: Pixels,

    /// Width of a single character cell
    pub cell_width: Pixels,

    /// Height of a single character cell (line height)
    pub cell_height: Pixels,

    /// Multiplier for line height to accommodate tall glyphs
    pub line_height_multiplier: f32,

    /// Color palette for resolving terminal colors
    pub palette: ColorPalette,

    /// Shared line cache for damage-aware rendering.
    /// Rc<RefCell<>> so the cache survives cloning into the canvas closure.
    line_cache: Rc<RefCell<LineCache>>,
}

impl TerminalRenderer {
    /// Creates a new terminal renderer with the given font settings and color palette.
    ///
    /// # Arguments
    ///
    /// * `font_family` - The name of the font family to use
    /// * `font_size` - The font size in pixels
    /// * `line_height_multiplier` - Multiplier for line height (e.g., 1.2 for 20% extra)
    /// * `palette` - The color palette to use for terminal colors
    ///
    /// # Returns
    ///
    /// A new `TerminalRenderer` instance with default cell dimensions.
    ///
    /// # Examples
    ///
    /// ```
    /// use gpui::px;
    /// use orcashell_terminal_view::renderer::TerminalRenderer;
    /// use orcashell_terminal_view::ColorPalette;
    ///
    /// let renderer = TerminalRenderer::new("Fira Code".to_string(), px(14.0), 1.0, ColorPalette::default());
    /// ```
    pub fn new(
        font_family: String,
        font_size: Pixels,
        line_height_multiplier: f32,
        palette: ColorPalette,
    ) -> Self {
        // Default cell dimensions - will be measured on first paint
        // Using 0.6 as approximate em-width ratio for monospace fonts
        let cell_width = font_size * 0.6;
        let cell_height = (font_size * 1.4).ceil(); // Line height with some spacing (ceiled to avoid sub-pixel gaps)

        let font_family_shared: SharedString = font_family.clone().into();
        Self {
            font_family,
            font_family_shared,
            font_size,
            cell_width,
            cell_height,
            line_height_multiplier,
            palette,
            line_cache: Rc::new(RefCell::new(LineCache::new())),
        }
    }

    /// Measure cell dimensions based on actual font metrics.
    ///
    /// This method measures the actual width and height of characters
    /// using the GPUI text system. It uses the '│' (BOX DRAWINGS LIGHT VERTICAL)
    /// character which spans the full cell height in properly designed terminal fonts.
    ///
    /// # Arguments
    ///
    /// * `window` - The GPUI window for text system access
    pub fn measure_cell(&mut self, window: &mut Window) {
        // Measure using '│' (U+2502, BOX DRAWINGS LIGHT VERTICAL)
        // This character spans the full cell height in terminal fonts, making it
        // ideal for measuring exact cell dimensions used by TUIs
        let font = Font {
            family: self.font_family_shared.clone(),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
        };

        let text_run = TextRun {
            len: "│".len(),
            font,
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        // Shape the box-drawing character to get cell metrics
        let shaped = window
            .text_system()
            .shape_line("│".into(), self.font_size, &[text_run], None);

        // Keep exact cell dimensions from font metrics.
        // Alignment is handled by floor()/ceil() at paint time (Okena pattern)
        // and by passing cell_width as advance override to shape_line.
        if shaped.width > px(0.0) {
            self.cell_width = shaped.width;
        }

        let line_height = (shaped.ascent + shaped.descent).ceil();
        if line_height > px(0.0) {
            // Ceil to whole pixel so rows tile without sub-pixel gaps
            // (prevents hairline seams through block characters like █)
            self.cell_height = (line_height * self.line_height_multiplier).ceil();
        }
    }

    /// Layout cells into batched text runs and background rects for a single row.
    ///
    /// This method processes a row of terminal cells and groups adjacent cells
    /// with identical styling into batched runs. It also collects background
    /// rectangles that need to be painted.
    ///
    /// # Arguments
    ///
    /// * `row` - The row number
    /// * `cells` - Iterator over (column, Cell) pairs
    /// * `colors` - Terminal color configuration
    ///
    /// # Returns
    ///
    /// A tuple of `(backgrounds, text_runs)` where:
    /// - `backgrounds` is a vector of merged background rectangles
    /// - `text_runs` is a vector of batched text runs
    pub fn layout_row(
        &self,
        row: usize,
        cells: &[(usize, Cell)],
        resolved_fg: &[Hsla],
        colors: &Colors,
    ) -> (Vec<BackgroundRect>, Vec<BatchedTextRun>) {
        debug_assert_eq!(cells.len(), resolved_fg.len());

        let mut backgrounds = Vec::with_capacity(cells.len());
        let mut text_runs = Vec::with_capacity(cells.len());

        let mut current_run: Option<BatchedTextRun> = None;
        let mut current_bg: Option<BackgroundRect> = None;

        for (idx, (col, cell)) in cells.iter().enumerate() {
            let col = *col;
            // Skip wide character spacers
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }

            // Extract cell styling
            let mut fg_color = resolved_fg[idx];
            let mut bg_color = self.palette.resolve(cell.bg, colors);
            let bold = cell.flags.contains(Flags::BOLD);
            let italic = cell.flags.contains(Flags::ITALIC);
            let mut underline_variant = if cell.flags.contains(Flags::UNDERCURL) {
                UnderlineVariant::Wavy
            } else if cell.flags.contains(Flags::DOUBLE_UNDERLINE) {
                UnderlineVariant::Double
            } else if cell.flags.contains(Flags::DOTTED_UNDERLINE) {
                UnderlineVariant::Dotted
            } else if cell.flags.contains(Flags::DASHED_UNDERLINE) {
                UnderlineVariant::Dashed
            } else if cell.flags.contains(Flags::UNDERLINE) {
                UnderlineVariant::Single
            } else {
                UnderlineVariant::None
            };
            let mut underline_color: Option<Hsla> = cell
                .underline_color()
                .map(|c| self.palette.resolve(c, colors));
            let has_hyperlink = cell.hyperlink().is_some();
            let strikethrough = cell.flags.contains(Flags::STRIKEOUT);

            // Hyperlink default styling: underline with link color when no explicit underline
            if has_hyperlink && underline_variant == UnderlineVariant::None {
                underline_variant = UnderlineVariant::Single;
                if underline_color.is_none() {
                    underline_color = Some(self.palette.link);
                }
            }

            // Swap foreground/background for inverse video (SGR 7).
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg_color, &mut bg_color);
            }

            // Dim: reduce foreground brightness (SGR 2).
            if cell.flags.contains(Flags::DIM) {
                fg_color.l *= 0.66;
            }

            // Hidden: make text invisible (SGR 8).
            if cell.flags.contains(Flags::HIDDEN) {
                fg_color = bg_color;
            }

            // Get the character (or space if empty / custom-rendered)
            // Box-drawing and block elements are rendered programmatically,
            // so replace them with spaces to prevent double-rendering if they
            // end up in a multi-character text run with matching styling.
            let ch = if cell.c == ' '
                || cell.c == '\0'
                || box_drawing::is_box_drawing_char(cell.c)
                || box_drawing::is_block_element(cell.c)
            {
                ' '
            } else {
                cell.c
            };

            // Handle background rectangles
            if let Some(ref mut bg_rect) = current_bg {
                if bg_rect.color == bg_color && bg_rect.end_col == col {
                    // Extend current background
                    bg_rect.end_col = col + 1;
                } else {
                    // Save current background and start new one
                    backgrounds.push(current_bg.take().unwrap());
                    current_bg = Some(BackgroundRect {
                        start_col: col,
                        end_col: col + 1,
                        row,
                        color: bg_color,
                    });
                }
            } else {
                // Start new background
                current_bg = Some(BackgroundRect {
                    start_col: col,
                    end_col: col + 1,
                    row,
                    color: bg_color,
                });
            }

            // Handle text runs
            if let Some(ref mut run) = current_run {
                if run.fg_color == fg_color
                    && run.bg_color == bg_color
                    && run.bold == bold
                    && run.italic == italic
                    && run.underline_variant == underline_variant
                    && run.underline_color == underline_color
                    && run.has_hyperlink == has_hyperlink
                    && run.strikethrough == strikethrough
                {
                    // Extend current run
                    run.text.push(ch);
                } else {
                    // Save current run and start new one
                    text_runs.push(current_run.take().unwrap());
                    let mut text = String::new();
                    text.push(ch);
                    current_run = Some(BatchedTextRun {
                        text,
                        start_col: col,
                        row,
                        fg_color,
                        bg_color,
                        bold,
                        italic,
                        underline_variant,
                        underline_color,
                        has_hyperlink,
                        strikethrough,
                    });
                }
            } else {
                // Start new run
                let mut text = String::new();
                text.push(ch);
                current_run = Some(BatchedTextRun {
                    text,
                    start_col: col,
                    row,
                    fg_color,
                    bg_color,
                    bold,
                    italic,
                    underline_variant,
                    underline_color,
                    has_hyperlink,
                    strikethrough,
                });
            }
        }

        // Push final run and background
        if let Some(run) = current_run {
            text_runs.push(run);
        }
        if let Some(bg) = current_bg {
            backgrounds.push(bg);
        }

        (backgrounds, text_runs)
    }

    /// Snapshot terminal state under lock for lock-free rendering.
    ///
    /// Consumes damage, clones cells for rows that need recomputation,
    /// copies scalar metadata, and resets damage. The caller should drop
    /// the terminal lock immediately after this returns.
    pub(crate) fn snapshot_frame<T: EventListener>(&self, term: &mut Term<T>) -> FrameSnapshot {
        let num_lines = term.grid().screen_lines();
        let num_cols = term.grid().columns();
        let display_offset = term.grid().display_offset() as i32;

        // Consume damage (requires &mut)
        let damage = term.damage();
        let mut damaged_set = vec![false; num_lines];
        let full_damage = match damage {
            TermDamage::Full => true,
            TermDamage::Partial(iter) => {
                for line_damage in iter {
                    if line_damage.line < num_lines {
                        damaged_set[line_damage.line] = true;
                    }
                }
                false
            }
        };

        // Validate cache to determine which rows need cell data
        let mut cache = self.line_cache.borrow_mut();
        let cache_valid = cache.validate(
            num_lines,
            num_cols,
            display_offset,
            self.font_size,
            self.palette.generation,
        );

        let mut row_cell_ranges: Vec<Option<RowCellRange>> = vec![None; num_lines];
        let row_needs_data: Vec<bool> = damaged_set
            .iter()
            .enumerate()
            .take(num_lines)
            .map(|(line_idx, &line_damaged)| {
                full_damage
                    || !cache_valid
                    || line_damaged
                    || cache.rows.get(line_idx).is_none_or(|r| r.is_none())
            })
            .collect();
        let rows_to_recompute = row_needs_data
            .iter()
            .filter(|&&needs_data| needs_data)
            .count();

        // Clone cells for rows that need recomputation into a flat buffer.
        let grid = term.grid();
        let mut cells = Vec::with_capacity(rows_to_recompute.saturating_mul(num_cols));
        for (line_idx, &needs_data) in row_needs_data.iter().enumerate().take(num_lines) {
            if needs_data {
                let start = cells.len();
                for col_idx in 0..num_cols {
                    let col = Column(col_idx);
                    let point = AlacPoint::new(Line(line_idx as i32 - display_offset), col);
                    cells.push((col_idx, grid[point].clone()));
                }
                row_cell_ranges[line_idx] = Some(RowCellRange {
                    start,
                    end: cells.len(),
                });
            }
        }
        drop(cache);

        // Copy scalar metadata
        let colors = *term.colors();
        let selection_range = term.selection.as_ref().and_then(|sel| sel.to_range(term));
        let cursor_point = term.grid().cursor.point;
        let term_mode = *term.mode();
        let history_size = term.grid().history_size();

        term.reset_damage();

        FrameSnapshot {
            num_lines,
            num_cols,
            display_offset,
            cells,
            row_cell_ranges,
            colors,
            selection_range,
            cursor_point,
            term_mode,
            history_size,
        }
    }

    /// Paint terminal content from a pre-captured snapshot.
    ///
    /// Performs layout, text shaping, cache updates, and painting without
    /// requiring the terminal lock.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_from_snapshot(
        &self,
        bounds: Bounds<Pixels>,
        padding: Edges<Pixels>,
        snapshot: &FrameSnapshot,
        cursor_visible: bool,
        cursor_shape: crate::terminal_view::CursorShape,
        visible_matches: &[crate::search::VisibleMatch],
        visible_hovered_link: Option<&crate::links::VisibleHoveredLink>,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let num_lines = snapshot.num_lines;
        let num_cols = snapshot.num_cols;
        let display_offset = snapshot.display_offset;
        let colors = &snapshot.colors;

        // Calculate default background color
        let default_bg = self.palette.resolve(
            Color::Named(alacritty_terminal::vte::ansi::NamedColor::Background),
            colors,
        );

        // Paint default background (covers full bounds including padding)
        window.paint_quad(quad(
            bounds,
            px(0.0),
            default_bg,
            Edges::<Pixels>::default(),
            transparent_black(),
            Default::default(),
        ));

        // Calculate origin offset (content starts after padding), snapped to pixel grid
        let origin = Point {
            x: (bounds.origin.x + padding.left).round(),
            y: (bounds.origin.y + padding.top).round(),
        };

        let mut cache = self.line_cache.borrow_mut();

        // Vertical offset to center text in cell (constant for a given font config)
        let base_height = self.cell_height / self.line_height_multiplier;
        let vertical_offset = (self.cell_height - base_height) / 2.0;

        // Flat vec for tracking which columns had horizontal box-drawing spans.
        let mut processed_horizontal = vec![false; num_cols];

        // Reusable buffers for Phase 1. Allocated once, cleared per row.
        let mut resolved_fg: Vec<Hsla> = Vec::with_capacity(num_cols);
        let mut box_draws: Vec<CachedBoxDraw> = Vec::new();
        let mut shaped_runs: Vec<CachedShapedRun> = Vec::new();
        let mut underlines: Vec<CachedUnderline> = Vec::new();

        // ── Phase 1: Cache Update ────────────────────────────────────────
        // Only rows with snapshot data are recomputed. Others keep cached state.
        for line_idx in 0..num_lines {
            let Some(cells) = snapshot.row_cells(line_idx) else {
                continue;
            };

            resolved_fg.clear();
            resolved_fg.extend(
                cells
                    .iter()
                    .map(|(_, cell)| self.palette.resolve(cell.fg, colors)),
            );

            // Layout → backgrounds + text_runs (text_runs are local, not cached)
            let (backgrounds, text_runs) = self.layout_row(line_idx, cells, &resolved_fg, colors);

            // Record box-drawing commands
            box_draws.clear();
            processed_horizontal.fill(false);

            // Effective fg for box-drawing: apply INVERSE/DIM/HIDDEN on demand
            // (only called for cells that are actually box-drawing/block elements).
            let effective_fg = |idx: usize, cell: &Cell| -> Hsla {
                let flags = cell.flags;
                if !flags.intersects(Flags::INVERSE | Flags::DIM | Flags::HIDDEN) {
                    return resolved_fg[idx];
                }
                let mut fg = resolved_fg[idx];
                let mut bg = self.palette.resolve(cell.bg, colors);
                if flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if flags.contains(Flags::DIM) {
                    fg.l *= 0.66;
                }
                if flags.contains(Flags::HIDDEN) {
                    fg = bg;
                }
                fg
            };

            let mut i = 0;
            while i < cells.len() {
                let (col_idx, ref cell) = cells[i];
                let ch = cell.c;

                if let Some(weight) = box_drawing::get_horizontal_weight(ch) {
                    let fg_color = effective_fg(i, cell);
                    let start_col = col_idx;
                    let mut end_col = col_idx;

                    let mut j = i + 1;
                    while j < cells.len() {
                        let (next_col, ref next_cell) = cells[j];
                        if next_col != end_col + 1 {
                            break;
                        }
                        let next_fg = effective_fg(j, next_cell);
                        if box_drawing::get_horizontal_weight(next_cell.c) == Some(weight)
                            && next_fg == fg_color
                        {
                            end_col = next_col;
                            j += 1;
                        } else {
                            break;
                        }
                    }

                    box_draws.push(CachedBoxDraw::HorizontalSpan {
                        start_col,
                        end_col_inclusive: end_col,
                        weight,
                        color: fg_color,
                    });

                    for flag in processed_horizontal
                        .iter_mut()
                        .take(end_col + 1)
                        .skip(start_col)
                    {
                        *flag = true;
                    }
                    i = j;
                    continue;
                }
                i += 1;
            }

            // Record vertical/full box-drawing characters
            for (idx, (col_idx, cell)) in cells.iter().enumerate() {
                let ch = cell.c;
                if ch == ' ' || ch == '\0' {
                    continue;
                }
                if box_drawing::is_block_element(ch) {
                    let fg_color = effective_fg(idx, cell);
                    box_draws.push(CachedBoxDraw::BlockElement {
                        col: *col_idx,
                        ch,
                        color: fg_color,
                    });
                } else if box_drawing::is_box_drawing_char(ch) {
                    let fg_color = effective_fg(idx, cell);
                    if processed_horizontal[*col_idx] {
                        box_draws.push(CachedBoxDraw::VerticalComponents {
                            col: *col_idx,
                            ch,
                            color: fg_color,
                        });
                    } else {
                        box_draws.push(CachedBoxDraw::FullChar {
                            col: *col_idx,
                            ch,
                            color: fg_color,
                        });
                    }
                }
            }

            // Shape text runs and cache with relative offsets
            shaped_runs.clear();
            for run in &text_runs {
                if run.text.chars().all(|c| c == ' ')
                    && run.underline_variant == UnderlineVariant::None
                    && !run.strikethrough
                {
                    continue;
                }
                if run.text.len() == 1 {
                    let c = run.text.chars().next().unwrap_or(' ');
                    if box_drawing::is_box_drawing_char(c) || box_drawing::is_block_element(c) {
                        continue;
                    }
                }

                let offset_x = self.cell_width * (run.start_col as f32);
                let offset_y = self.cell_height * (line_idx as f32) + vertical_offset;

                let font = Font {
                    family: self.font_family_shared.clone(),
                    features: FontFeatures::default(),
                    fallbacks: None,
                    weight: if run.bold {
                        FontWeight::BOLD
                    } else {
                        FontWeight::NORMAL
                    },
                    style: if run.italic {
                        FontStyle::Italic
                    } else {
                        FontStyle::Normal
                    },
                };

                let ul_resolved = run.underline_color.unwrap_or(run.fg_color);

                // Route underline: Single/Wavy use GPUI native; Double/Dotted/Dashed use custom paint
                let underline = match run.underline_variant {
                    UnderlineVariant::Single => Some(UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(ul_resolved),
                        wavy: false,
                    }),
                    UnderlineVariant::Wavy => Some(UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(ul_resolved),
                        wavy: true,
                    }),
                    UnderlineVariant::Double => {
                        let end_col = run.start_col + run.text.chars().count();
                        underlines.push(CachedUnderline::Double {
                            start_col: run.start_col,
                            end_col,
                            color: ul_resolved,
                        });
                        None
                    }
                    UnderlineVariant::Dotted => {
                        let end_col = run.start_col + run.text.chars().count();
                        underlines.push(CachedUnderline::Dotted {
                            start_col: run.start_col,
                            end_col,
                            color: ul_resolved,
                        });
                        None
                    }
                    UnderlineVariant::Dashed => {
                        let end_col = run.start_col + run.text.chars().count();
                        underlines.push(CachedUnderline::Dashed {
                            start_col: run.start_col,
                            end_col,
                            color: ul_resolved,
                        });
                        None
                    }
                    UnderlineVariant::None => None,
                };

                let text_run = TextRun {
                    len: run.text.len(),
                    font,
                    color: run.fg_color,
                    background_color: None,
                    underline,
                    strikethrough: if run.strikethrough {
                        Some(StrikethroughStyle {
                            thickness: px(1.0),
                            color: Some(run.fg_color),
                        })
                    } else {
                        None
                    },
                };

                let text: SharedString = run.text.clone().into();
                let shaped_line = window.text_system().shape_line(
                    text,
                    self.font_size,
                    &[text_run],
                    Some(self.cell_width),
                );

                shaped_runs.push(CachedShapedRun {
                    shaped_line,
                    offset_x,
                    offset_y,
                });
            }

            cache.rows[line_idx] = Some(CachedRowLayout {
                backgrounds,
                shaped_runs: std::mem::take(&mut shaped_runs),
                box_draws: std::mem::take(&mut box_draws),
                underlines: std::mem::take(&mut underlines),
            });
        }

        // ── Phase 2: Paint Replay ────────────────────────────────────────
        // All rows paint from cache. No cloning. We borrow directly.
        let cw: f32 = self.cell_width.into();

        for line_idx in 0..num_lines {
            let cached = cache.rows[line_idx]
                .as_ref()
                .expect("Phase 1 should have populated every visible row");

            // Paint backgrounds
            for bg_rect in &cached.backgrounds {
                if bg_rect.color == default_bg {
                    continue;
                }
                let x = px((f32::from(origin.x) + cw * bg_rect.start_col as f32).floor());
                let y = origin.y + self.cell_height * (bg_rect.row as f32);
                let width = px((cw * (bg_rect.end_col - bg_rect.start_col) as f32).ceil());

                window.paint_quad(quad(
                    Bounds {
                        origin: Point { x, y },
                        size: Size {
                            width,
                            height: self.cell_height,
                        },
                    },
                    px(0.0),
                    bg_rect.color,
                    Edges::<Pixels>::default(),
                    transparent_black(),
                    Default::default(),
                ));
            }

            // Paint selection highlight overlay
            if let Some(ref range) = snapshot.selection_range {
                if let Some((start, sel_end)) =
                    selection_span_for_row(range, line_idx, display_offset, num_cols)
                {
                    let x = px((f32::from(origin.x) + cw * start as f32).floor());
                    let y = origin.y + self.cell_height * (line_idx as f32);
                    let width = px((cw * (sel_end - start) as f32).ceil());
                    let highlight = self.palette.terminal_selection;
                    window.paint_quad(quad(
                        Bounds {
                            origin: Point { x, y },
                            size: Size {
                                width,
                                height: self.cell_height,
                            },
                        },
                        px(0.0),
                        highlight,
                        Edges::<Pixels>::default(),
                        transparent_black(),
                        Default::default(),
                    ));
                }
            }

            // Paint search match highlight overlays
            for search_match in visible_matches {
                if let Some((start, match_end)) =
                    search_span_for_row(search_match, line_idx, display_offset, num_cols)
                {
                    let x = px((f32::from(origin.x) + cw * start as f32).floor());
                    let y = origin.y + self.cell_height * (line_idx as f32);
                    let width = px((cw * (match_end - start) as f32).ceil());
                    let highlight_color = if search_match.is_current {
                        self.palette.search_match_active
                    } else {
                        self.palette.search_match_other
                    };
                    window.paint_quad(quad(
                        Bounds {
                            origin: Point { x, y },
                            size: Size {
                                width,
                                height: self.cell_height,
                            },
                        },
                        px(0.0),
                        highlight_color,
                        Edges::<Pixels>::default(),
                        transparent_black(),
                        Default::default(),
                    ));
                }
            }

            // Replay box-drawing commands
            let y_base = origin.y + self.cell_height * (line_idx as f32);
            let cy = y_base + self.cell_height / 2.0;

            for bd in &cached.box_draws {
                match bd {
                    CachedBoxDraw::HorizontalSpan {
                        start_col,
                        end_col_inclusive,
                        weight,
                        color,
                    } => {
                        let start_x = origin.x + self.cell_width * (*start_col as f32);
                        let end_x = origin.x + self.cell_width * ((*end_col_inclusive + 1) as f32);
                        box_drawing::draw_horizontal_span(
                            start_x,
                            end_x,
                            cy,
                            *weight,
                            self.cell_width,
                            *color,
                            window,
                        );
                    }
                    CachedBoxDraw::VerticalComponents { col, ch, color } => {
                        let x = origin.x + self.cell_width * (*col as f32);
                        let cell_bounds = Bounds {
                            origin: Point { x, y: y_base },
                            size: Size {
                                width: self.cell_width,
                                height: self.cell_height,
                            },
                        };
                        box_drawing::draw_vertical_components(
                            *ch,
                            cell_bounds,
                            *color,
                            self.cell_width,
                            window,
                        );
                    }
                    CachedBoxDraw::FullChar { col, ch, color } => {
                        let x = origin.x + self.cell_width * (*col as f32);
                        let cell_bounds = Bounds {
                            origin: Point { x, y: y_base },
                            size: Size {
                                width: self.cell_width,
                                height: self.cell_height,
                            },
                        };
                        box_drawing::draw_box_character(
                            *ch,
                            cell_bounds,
                            *color,
                            self.cell_width,
                            window,
                        );
                    }
                    CachedBoxDraw::BlockElement { col, ch, color } => {
                        let x = origin.x + self.cell_width * (*col as f32);
                        let cell_bounds = Bounds {
                            origin: Point { x, y: y_base },
                            size: Size {
                                width: self.cell_width,
                                height: self.cell_height,
                            },
                        };
                        box_drawing::draw_block_element(*ch, cell_bounds, *color, window);
                    }
                }
            }

            // Replay custom underlines (double, dotted, dashed)
            for ul in &cached.underlines {
                let (start_col, end_col, color) = match ul {
                    CachedUnderline::Double {
                        start_col,
                        end_col,
                        color,
                    }
                    | CachedUnderline::Dotted {
                        start_col,
                        end_col,
                        color,
                    }
                    | CachedUnderline::Dashed {
                        start_col,
                        end_col,
                        color,
                    } => (*start_col, *end_col, *color),
                };
                let x_start = px((f32::from(origin.x) + cw * start_col as f32).floor());
                let span_width = px((cw * (end_col - start_col) as f32).ceil());
                // Scale underline position and gap with cell height
                let ul_offset = (self.cell_height * 0.1).max(px(2.0));
                let underline_y = y_base + self.cell_height - ul_offset;
                let thickness = px(1.0);
                let double_gap = (self.cell_height * 0.15).max(px(3.0));

                match ul {
                    CachedUnderline::Double { .. } => {
                        // Two parallel lines
                        window.paint_quad(quad(
                            Bounds {
                                origin: Point {
                                    x: x_start,
                                    y: underline_y,
                                },
                                size: Size {
                                    width: span_width,
                                    height: thickness,
                                },
                            },
                            px(0.0),
                            color,
                            Edges::<Pixels>::default(),
                            transparent_black(),
                            Default::default(),
                        ));
                        window.paint_quad(quad(
                            Bounds {
                                origin: Point {
                                    x: x_start,
                                    y: underline_y - double_gap,
                                },
                                size: Size {
                                    width: span_width,
                                    height: thickness,
                                },
                            },
                            px(0.0),
                            color,
                            Edges::<Pixels>::default(),
                            transparent_black(),
                            Default::default(),
                        ));
                    }
                    CachedUnderline::Dotted { .. } => {
                        // 1px dots with 2px gaps
                        let dot_size = px(1.0);
                        let step: f32 = 3.0;
                        let mut dx: f32 = 0.0;
                        let span_f: f32 = span_width.into();
                        while dx < span_f {
                            window.paint_quad(quad(
                                Bounds {
                                    origin: Point {
                                        x: x_start + px(dx),
                                        y: underline_y,
                                    },
                                    size: Size {
                                        width: dot_size,
                                        height: thickness,
                                    },
                                },
                                px(0.0),
                                color,
                                Edges::<Pixels>::default(),
                                transparent_black(),
                                Default::default(),
                            ));
                            dx += step;
                        }
                    }
                    CachedUnderline::Dashed { .. } => {
                        // 4px dashes with 2px gaps
                        let dash_len = px(4.0);
                        let step: f32 = 6.0;
                        let mut dx: f32 = 0.0;
                        let span_f: f32 = span_width.into();
                        while dx < span_f {
                            let remaining = span_f - dx;
                            let w = if remaining < 4.0 {
                                px(remaining)
                            } else {
                                dash_len
                            };
                            window.paint_quad(quad(
                                Bounds {
                                    origin: Point {
                                        x: x_start + px(dx),
                                        y: underline_y,
                                    },
                                    size: Size {
                                        width: w,
                                        height: thickness,
                                    },
                                },
                                px(0.0),
                                color,
                                Edges::<Pixels>::default(),
                                transparent_black(),
                                Default::default(),
                            ));
                            dx += step;
                        }
                    }
                }
            }

            if let Some(hovered_link) = visible_hovered_link {
                if let Some((start_col, end_col)) =
                    hovered_link_span_for_row(hovered_link, line_idx, display_offset, num_cols)
                {
                    let x_start = px((f32::from(origin.x) + cw * start_col as f32).floor());
                    let span_width = px((cw * (end_col - start_col) as f32).ceil());
                    let ul_offset = (self.cell_height * 0.1).max(px(2.0));
                    let underline_y = y_base + self.cell_height - ul_offset;
                    window.paint_quad(quad(
                        Bounds {
                            origin: Point {
                                x: x_start,
                                y: underline_y,
                            },
                            size: Size {
                                width: span_width,
                                height: px(1.0),
                            },
                        },
                        px(0.0),
                        hovered_link.underline_color.unwrap_or(self.palette.link),
                        Edges::<Pixels>::default(),
                        transparent_black(),
                        Default::default(),
                    ));
                }
            }

            // Replay shaped text (no shaping call. Just paint cached ShapedLine)
            for sr in &cached.shaped_runs {
                let paint_x = (origin.x + sr.offset_x).round();
                let paint_y = (origin.y + sr.offset_y).round();
                let _ = sr.shaped_line.paint(
                    Point {
                        x: paint_x,
                        y: paint_y,
                    },
                    self.cell_height,
                    window,
                    _cx,
                );
            }
        }

        // Paint cursor: hide when scrolled back, respect blink timer and SHOW_CURSOR mode
        let term_shows_cursor = snapshot.term_mode.contains(TermMode::SHOW_CURSOR);
        if cursor_visible && term_shows_cursor && display_offset == 0 {
            let cursor_point = snapshot.cursor_point;
            let cw: f32 = self.cell_width.into();
            let ch: f32 = self.cell_height.into();
            let cursor_x = px((f32::from(origin.x) + cw * cursor_point.column.0 as f32).floor());
            let cursor_y = px((f32::from(origin.y) + ch * cursor_point.line.0 as f32).floor());

            let cursor_color = self.palette.resolve(
                Color::Named(alacritty_terminal::vte::ansi::NamedColor::Cursor),
                colors,
            );

            use crate::terminal_view::CursorShape;
            let (cursor_w, cursor_h, cursor_y_offset, corner_radius) = match cursor_shape {
                CursorShape::Bar => {
                    let w = self.cell_width * 0.3;
                    (w, self.cell_height, px(0.0), w / 2.0)
                }
                CursorShape::Block => (self.cell_width, self.cell_height, px(0.0), px(0.0)),
                CursorShape::Underline => {
                    let h = px(2.0);
                    (self.cell_width, h, self.cell_height - h, px(0.0))
                }
            };

            let cursor_bounds = Bounds {
                origin: Point {
                    x: cursor_x,
                    y: cursor_y + cursor_y_offset,
                },
                size: Size {
                    width: cursor_w,
                    height: cursor_h,
                },
            };

            window.paint_quad(quad(
                cursor_bounds,
                corner_radius,
                cursor_color,
                Edges::<Pixels>::default(),
                transparent_black(),
                Default::default(),
            ));
        }

        // Paint scrollbar only when scrolled into history
        let track_height: f32 = bounds.size.height.into();
        if display_offset > 0 {
            if let Some(geom) = scrollbar_geometry(
                num_lines,
                snapshot.history_size,
                display_offset.max(0) as usize,
                track_height,
            ) {
                let bar_width = px(SCROLLBAR_VISUAL_WIDTH);
                let bar_x =
                    bounds.origin.x + bounds.size.width - bar_width - px(SCROLLBAR_RIGHT_MARGIN);

                let bar_bounds = Bounds {
                    origin: Point {
                        x: bar_x,
                        y: bounds.origin.y + px(geom.thumb_y),
                    },
                    size: Size {
                        width: bar_width,
                        height: px(geom.thumb_height),
                    },
                };

                let bar_color = self.palette.scrollbar;
                window.paint_quad(quad(
                    bar_bounds,
                    px(2.0),
                    bar_color,
                    Edges::<Pixels>::default(),
                    transparent_black(),
                    Default::default(),
                ));
            }
        }
    }
}

/// Visual width of the scrollbar thumb in pixels.
pub(crate) const SCROLLBAR_VISUAL_WIDTH: f32 = 4.0;

/// Right-edge margin for the scrollbar in pixels.
pub(crate) const SCROLLBAR_RIGHT_MARGIN: f32 = 2.0;

/// Hit-test width for scrollbar interaction (wider than visual for easier clicking).
pub(crate) const SCROLLBAR_HIT_WIDTH: f32 = 10.0;

/// Scrollbar thumb geometry computed from terminal scroll state.
pub(crate) struct ScrollbarGeometry {
    /// Y offset of the thumb from the top of the track, in pixels.
    pub thumb_y: f32,
    /// Height of the thumb in pixels.
    pub thumb_height: f32,
}

/// Calculate scrollbar geometry from terminal metrics.
///
/// Returns `None` if there is no scrollable content (`history_size == 0`).
pub(crate) fn scrollbar_geometry(
    num_lines: usize,
    history_size: usize,
    display_offset: usize,
    track_height: f32,
) -> Option<ScrollbarGeometry> {
    if history_size == 0 {
        return None;
    }
    let total_lines = num_lines + history_size;
    let visible_frac = num_lines as f32 / total_lines as f32;
    let offset_frac = display_offset as f32 / history_size.max(1) as f32;
    let thumb_height = (visible_frac * track_height).max(20.0);
    let max_travel = track_height - thumb_height;
    let thumb_y = (1.0 - offset_frac) * max_travel;
    Some(ScrollbarGeometry {
        thumb_y,
        thumb_height,
    })
}

/// Compute the selected column range for a given visual row in O(1).
/// Returns `(start_col, end_col_exclusive)` or `None` if no selection on this row.
fn selection_span_for_row(
    range: &SelectionRange,
    visual_line: usize,
    display_offset: i32,
    num_cols: usize,
) -> Option<(usize, usize)> {
    let buffer_line = Line(visual_line as i32 - display_offset);

    if buffer_line < range.start.line || buffer_line > range.end.line {
        return None;
    }

    let (start_col, end_col) = if range.is_block {
        (range.start.column.0, range.end.column.0 + 1)
    } else if range.start.line == range.end.line {
        // Single-line selection
        (range.start.column.0, range.end.column.0 + 1)
    } else if buffer_line == range.start.line {
        // First line of multi-line selection
        (range.start.column.0, num_cols)
    } else if buffer_line == range.end.line {
        // Last line of multi-line selection
        (0, range.end.column.0 + 1)
    } else {
        // Middle line: full row selected
        (0, num_cols)
    };

    Some((start_col.min(num_cols), end_col.min(num_cols)))
}

/// Compute the search match column range for a given visual row in O(1).
/// Returns `(start_col, end_col_exclusive)` or `None` if no match on this row.
fn search_span_for_row(
    search_match: &crate::search::VisibleMatch,
    visual_line: usize,
    display_offset: i32,
    num_cols: usize,
) -> Option<(usize, usize)> {
    let buffer_line = Line(visual_line as i32 - display_offset);
    let match_start = *search_match.range.start();
    let match_end = *search_match.range.end();

    if buffer_line < match_start.line || buffer_line > match_end.line {
        return None;
    }

    let start_col = if buffer_line == match_start.line {
        match_start.column.0
    } else {
        0
    };

    let end_col = if buffer_line == match_end.line {
        match_end.column.0 + 1
    } else {
        num_cols
    };

    Some((start_col.min(num_cols), end_col.min(num_cols)))
}

fn hovered_link_span_for_row(
    hovered_link: &crate::links::VisibleHoveredLink,
    visual_line: usize,
    display_offset: i32,
    num_cols: usize,
) -> Option<(usize, usize)> {
    let buffer_line = Line(visual_line as i32 - display_offset);
    let link_start = *hovered_link.range.start();
    let link_end = *hovered_link.range.end();

    if buffer_line < link_start.line || buffer_line > link_end.line {
        return None;
    }

    let start_col = if buffer_line == link_start.line {
        link_start.column.0
    } else {
        0
    };

    let end_col = if buffer_line == link_end.line {
        link_end.column.0 + 1
    } else {
        num_cols
    };

    Some((start_col.min(num_cols), end_col.min(num_cols)))
}

#[cfg(test)]
mod tests {
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
            let analytical =
                hovered_link_span_for_row(&hovered, visual_line, display_offset, num_cols);

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
}
