//! Color palette for terminal emulator.
//!
//! This module provides [`ColorPalette`] and [`ColorPaletteBuilder`] for managing
//! terminal colors. It supports:
//!
//! - **16 ANSI colors**: Orca Brutalism pastel neon palette by default
//! - **256-color mode**: Extended palette with 6x6x6 RGB cube and grayscale ramp
//! - **True color (24-bit RGB)**: Direct RGB color specification
//! - **UI colors**: Search bar, scrollbar, hover overlay colors used by terminal-view
//!
//! # Default ANSI Colors (Pastel Neons)
//!
//! | Index | Name | RGB |
//! |-------|------|-----|
//! | 0 | Abyss | `#0A0C10` |
//! | 1 | Neon Coral | `#FF7E9D` |
//! | 2 | Neon Mint | `#7EFFC1` |
//! | 3 | Neon Amber | `#FFD97E` |
//! | 4 | Orca Blue | `#5E9BFF` |
//! | 5 | Neon Lavender | `#B87EFF` |
//! | 6 | Neon Cyan | `#7EE8FA` |
//! | 7 | Bone | `#D8DAE0` |
//! | 8 | Slate | `#5C6070` |
//! | 9 | Bright Coral | `#FFA0B8` |
//! | 10 | Bright Mint | `#A0FFD6` |
//! | 11 | Bright Amber | `#FFE5A0` |
//! | 12 | Bright Blue | `#82B4FF` |
//! | 13 | Bright Lavender | `#CCA0FF` |
//! | 14 | Bright Cyan | `#A0F0FF` |
//! | 15 | Patch | `#E8EAF0` |
//!
//! # 256-Color Mode
//!
//! Colors 16-255 are calculated:
//!
//! - **16-231**: 6x6x6 RGB cube where each component is `0, 95, 135, 175, 215, 255`
//! - **232-255**: 24-step grayscale from `#080808` to `#EEEEEE`
//!
//! # Example
//!
//! ```
//! use orcashell_terminal_view::ColorPalette;
//!
//! // Use default palette (pastel neons)
//! let default = ColorPalette::default();
//!
//! // Or customize with builder
//! let custom = ColorPalette::builder()
//!     .background(0x1a, 0x1b, 0x26)
//!     .foreground(0xa9, 0xb1, 0xd6)
//!     .red(0xf7, 0x76, 0x8e)
//!     .green(0x9e, 0xce, 0x6a)
//!     .blue(0x7a, 0xa2, 0xf7)
//!     .build();
//! ```

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::Hsla;
use std::sync::atomic::{AtomicU64, Ordering};

/// A color palette that maps ANSI colors to GPUI Hsla colors.
///
/// This struct maintains the 16-color ANSI palette, 256-color extended palette,
/// special colors (foreground, background, cursor), and UI colors used by
/// terminal-view components (search bar, scrollbar, hover overlays).
#[derive(Debug, Clone)]
pub struct ColorPalette {
    // ── Terminal colors ───────────────────────────────────────────────
    ansi_colors: [Hsla; 16],
    extended_colors: [Hsla; 256],
    foreground: Hsla,
    background: Hsla,
    cursor: Hsla,

    // ── UI colors (used by terminal-view components) ──────────────────
    /// Search match highlight - current match (ORCA_BLUE at 40%).
    pub search_match_active: Hsla,
    /// Search match highlight - other matches (ORCA_BLUE at 18%).
    pub search_match_other: Hsla,
    /// Scrollbar color (ORCA_BLUE at 50%).
    pub scrollbar: Hsla,
    /// Search bar background (SURFACE).
    pub search_bar_bg: Hsla,
    /// Search bar border (ORCA_BLUE at 25%).
    pub search_bar_border: Hsla,
    /// Search bar text (FOG).
    pub search_bar_text: Hsla,
    /// Search input background (ABYSS).
    pub search_input_bg: Hsla,
    /// Search input text (BONE).
    pub search_input_text: Hsla,
    /// Search input placeholder (SLATE at 50%).
    pub search_input_placeholder: Hsla,
    /// Search input cursor (ORCA_BLUE).
    pub search_input_cursor: Hsla,
    /// Search input selection background (ORCA_BLUE at 25%).
    pub search_input_selection: Hsla,
    /// Hover overlay for buttons (white-ish at ~6%).
    pub hover_overlay: Hsla,
    /// Close button hover overlay (red-ish at ~25%).
    pub close_hover_overlay: Hsla,
    /// Hyperlink text color and default underline (ORCA_BLUE).
    pub link: Hsla,

    /// Cache generation counter. Bumped when colors change so the renderer
    /// can invalidate cached ShapedLine data that carries color information.
    pub generation: u64,
}

static NEXT_PALETTE_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_palette_generation() -> u64 {
    NEXT_PALETTE_GENERATION.fetch_add(1, Ordering::Relaxed)
}

impl Default for ColorPalette {
    fn default() -> Self {
        // Orca Brutalism pastel neon ANSI palette
        let ansi_colors = [
            // Normal colors
            rgb_to_hsla(Rgb {
                r: 0x0A,
                g: 0x0C,
                b: 0x10,
            }), // 0  Abyss (black)
            rgb_to_hsla(Rgb {
                r: 0xFF,
                g: 0x7E,
                b: 0x9D,
            }), // 1  Neon Coral
            rgb_to_hsla(Rgb {
                r: 0x7E,
                g: 0xFF,
                b: 0xC1,
            }), // 2  Neon Mint
            rgb_to_hsla(Rgb {
                r: 0xFF,
                g: 0xD9,
                b: 0x7E,
            }), // 3  Neon Amber
            rgb_to_hsla(Rgb {
                r: 0x5E,
                g: 0x9B,
                b: 0xFF,
            }), // 4  Orca Blue
            rgb_to_hsla(Rgb {
                r: 0xB8,
                g: 0x7E,
                b: 0xFF,
            }), // 5  Neon Lavender
            rgb_to_hsla(Rgb {
                r: 0x7E,
                g: 0xE8,
                b: 0xFA,
            }), // 6  Neon Cyan
            rgb_to_hsla(Rgb {
                r: 0xD8,
                g: 0xDA,
                b: 0xE0,
            }), // 7  Bone (white)
            // Bright colors
            rgb_to_hsla(Rgb {
                r: 0x5C,
                g: 0x60,
                b: 0x70,
            }), // 8  Slate (bright black)
            rgb_to_hsla(Rgb {
                r: 0xFF,
                g: 0xA0,
                b: 0xB8,
            }), // 9  Bright Coral
            rgb_to_hsla(Rgb {
                r: 0xA0,
                g: 0xFF,
                b: 0xD6,
            }), // 10 Bright Mint
            rgb_to_hsla(Rgb {
                r: 0xFF,
                g: 0xE5,
                b: 0xA0,
            }), // 11 Bright Amber
            rgb_to_hsla(Rgb {
                r: 0x82,
                g: 0xB4,
                b: 0xFF,
            }), // 12 Bright Blue
            rgb_to_hsla(Rgb {
                r: 0xCC,
                g: 0xA0,
                b: 0xFF,
            }), // 13 Bright Lavender
            rgb_to_hsla(Rgb {
                r: 0xA0,
                g: 0xF0,
                b: 0xFF,
            }), // 14 Bright Cyan
            rgb_to_hsla(Rgb {
                r: 0xE8,
                g: 0xEA,
                b: 0xF0,
            }), // 15 Patch (bright white)
        ];

        // Build the full 256-color palette
        let mut extended_colors = [Hsla::default(); 256];
        extended_colors[0..16].copy_from_slice(&ansi_colors);

        // Colors 16-231: 6x6x6 RGB cube
        let mut idx = 16;
        for r in 0..6 {
            for g in 0..6 {
                for b in 0..6 {
                    let rgb = Rgb {
                        r: if r == 0 { 0 } else { 55 + r * 40 },
                        g: if g == 0 { 0 } else { 55 + g * 40 },
                        b: if b == 0 { 0 } else { 55 + b * 40 },
                    };
                    extended_colors[idx] = rgb_to_hsla(rgb);
                    idx += 1;
                }
            }
        }

        // Colors 232-255: Grayscale ramp
        for i in 0..24 {
            let gray = (8 + i * 10) as u8;
            extended_colors[232 + i] = rgb_to_hsla(Rgb {
                r: gray,
                g: gray,
                b: gray,
            });
        }

        // Default terminal colors (overridden by build_terminal_config in practice)
        let foreground = rgb_to_hsla(Rgb {
            r: 0xd4,
            g: 0xd4,
            b: 0xd4,
        });
        let background = rgb_to_hsla(Rgb {
            r: 0x1e,
            g: 0x1e,
            b: 0x1e,
        });
        let cursor = rgb_to_hsla(Rgb {
            r: 0xff,
            g: 0xff,
            b: 0xff,
        });

        Self {
            ansi_colors,
            extended_colors,
            foreground,
            background,
            cursor,

            // UI colors. Current hardcoded values, no visual change
            search_match_active: gpui::rgba(0x5E9BFF66).into(),
            search_match_other: gpui::rgba(0x5E9BFF2E).into(),
            scrollbar: gpui::rgba(0x5E9BFF4D).into(),
            search_bar_bg: gpui::rgba(0x262A34FF).into(),
            search_bar_border: gpui::rgba(0x5E9BFF40).into(),
            search_bar_text: gpui::rgba(0x9499A8FF).into(),
            search_input_bg: gpui::rgba(0x1C1F26FF).into(),
            search_input_text: gpui::rgba(0xD8DAE0FF).into(),
            search_input_placeholder: gpui::rgba(0x5C607080).into(),
            search_input_cursor: gpui::rgba(0x5E9BFFFF).into(),
            search_input_selection: gpui::rgba(0x5E9BFF40).into(),
            hover_overlay: gpui::rgba(0xFFFFFF10).into(),
            close_hover_overlay: gpui::rgba(0xF14C4C40).into(),
            link: gpui::rgba(0x5E9BFFFF).into(),

            generation: 0,
        }
    }
}

impl ColorPalette {
    /// Creates a new color palette with default colors.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new color palette builder for customizing colors.
    pub fn builder() -> ColorPaletteBuilder {
        ColorPaletteBuilder::new()
    }

    /// Resolves a terminal color to a GPUI Hsla color.
    pub fn resolve(&self, color: Color, colors: &Colors) -> Hsla {
        match color {
            Color::Named(named) => {
                if let Some(rgb) = colors[named] {
                    return rgb_to_hsla(rgb);
                }

                let idx = named as usize;
                if idx < 16 {
                    self.ansi_colors[idx]
                } else {
                    match named {
                        NamedColor::Foreground => self.foreground,
                        NamedColor::Background => self.background,
                        NamedColor::Cursor => self.cursor,
                        NamedColor::DimForeground => {
                            let mut dim = self.foreground;
                            dim.l *= 0.7;
                            dim
                        }
                        NamedColor::BrightForeground => {
                            let mut bright = self.foreground;
                            bright.l = (bright.l * 1.2).min(1.0);
                            bright
                        }
                        NamedColor::DimBlack
                        | NamedColor::DimRed
                        | NamedColor::DimGreen
                        | NamedColor::DimYellow
                        | NamedColor::DimBlue
                        | NamedColor::DimMagenta
                        | NamedColor::DimCyan
                        | NamedColor::DimWhite => {
                            let base_idx = match named {
                                NamedColor::DimBlack => 0,
                                NamedColor::DimRed => 1,
                                NamedColor::DimGreen => 2,
                                NamedColor::DimYellow => 3,
                                NamedColor::DimBlue => 4,
                                NamedColor::DimMagenta => 5,
                                NamedColor::DimCyan => 6,
                                NamedColor::DimWhite => 7,
                                _ => 7,
                            };
                            let mut dim = self.ansi_colors[base_idx];
                            dim.l *= 0.7;
                            dim
                        }
                        _ => self.foreground,
                    }
                }
            }
            Color::Spec(rgb) => rgb_to_hsla(rgb),
            Color::Indexed(idx) => self.extended_colors[idx as usize],
        }
    }

    pub fn ansi_colors(&self) -> &[Hsla; 16] {
        &self.ansi_colors
    }

    pub fn extended_colors(&self) -> &[Hsla; 256] {
        &self.extended_colors
    }

    pub fn foreground(&self) -> Hsla {
        self.foreground
    }

    pub fn background(&self) -> Hsla {
        self.background
    }

    pub fn cursor(&self) -> Hsla {
        self.cursor
    }
}

/// Converts an RGB color to GPUI's Hsla color format.
pub fn rgb_to_hsla(rgb: Rgb) -> Hsla {
    let r = rgb.r as f32 / 255.0;
    let g = rgb.g as f32 / 255.0;
    let b = rgb.b as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let l = (max + min) / 2.0;

    let s = if delta == 0.0 {
        0.0
    } else {
        delta / (1.0 - (2.0 * l - 1.0).abs())
    };

    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };

    let h = if h < 0.0 { h + 360.0 } else { h } / 360.0;

    Hsla { h, s, l, a: 1.0 }
}

/// Builder for creating a customized color palette.
#[derive(Debug, Clone)]
pub struct ColorPaletteBuilder {
    palette: ColorPalette,
}

impl Default for ColorPaletteBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ColorPaletteBuilder {
    pub fn new() -> Self {
        Self {
            palette: ColorPalette::default(),
        }
    }

    pub fn background(mut self, r: u8, g: u8, b: u8) -> Self {
        self.palette.background = rgb_to_hsla(Rgb { r, g, b });
        self
    }

    pub fn foreground(mut self, r: u8, g: u8, b: u8) -> Self {
        self.palette.foreground = rgb_to_hsla(Rgb { r, g, b });
        self
    }

    pub fn cursor(mut self, r: u8, g: u8, b: u8) -> Self {
        self.palette.cursor = rgb_to_hsla(Rgb { r, g, b });
        self
    }

    pub fn black(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(0, r, g, b);
        self
    }

    pub fn black_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(0, hex);
        self
    }

    pub fn red(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(1, r, g, b);
        self
    }

    pub fn red_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(1, hex);
        self
    }

    pub fn green(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(2, r, g, b);
        self
    }

    pub fn green_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(2, hex);
        self
    }

    pub fn yellow(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(3, r, g, b);
        self
    }

    pub fn yellow_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(3, hex);
        self
    }

    pub fn blue(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(4, r, g, b);
        self
    }

    pub fn blue_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(4, hex);
        self
    }

    pub fn magenta(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(5, r, g, b);
        self
    }

    pub fn magenta_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(5, hex);
        self
    }

    pub fn cyan(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(6, r, g, b);
        self
    }

    pub fn cyan_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(6, hex);
        self
    }

    pub fn white(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(7, r, g, b);
        self
    }

    pub fn white_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(7, hex);
        self
    }

    pub fn bright_black(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(8, r, g, b);
        self
    }

    pub fn bright_black_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(8, hex);
        self
    }

    pub fn bright_red(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(9, r, g, b);
        self
    }

    pub fn bright_red_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(9, hex);
        self
    }

    pub fn bright_green(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(10, r, g, b);
        self
    }

    pub fn bright_green_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(10, hex);
        self
    }

    pub fn bright_yellow(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(11, r, g, b);
        self
    }

    pub fn bright_yellow_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(11, hex);
        self
    }

    pub fn bright_blue(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(12, r, g, b);
        self
    }

    pub fn bright_blue_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(12, hex);
        self
    }

    pub fn bright_magenta(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(13, r, g, b);
        self
    }

    pub fn bright_magenta_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(13, hex);
        self
    }

    pub fn bright_cyan(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(14, r, g, b);
        self
    }

    pub fn bright_cyan_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(14, hex);
        self
    }

    pub fn bright_white(mut self, r: u8, g: u8, b: u8) -> Self {
        self.set_ansi_color(15, r, g, b);
        self
    }

    pub fn bright_white_channels(mut self, hex: u32) -> Self {
        self.set_ansi_hex(15, hex);
        self
    }

    pub fn link(mut self, r: u8, g: u8, b: u8) -> Self {
        self.palette.link = rgb_to_hsla(Rgb { r, g, b });
        self
    }

    fn set_ansi_color(&mut self, idx: usize, r: u8, g: u8, b: u8) {
        let color = rgb_to_hsla(Rgb { r, g, b });
        self.palette.ansi_colors[idx] = color;
        self.palette.extended_colors[idx] = color;
        self.palette.generation += 1;
    }

    fn set_ansi_hex(&mut self, idx: usize, hex: u32) {
        self.set_ansi_color(
            idx,
            ((hex >> 16) & 0xFF) as u8,
            ((hex >> 8) & 0xFF) as u8,
            (hex & 0xFF) as u8,
        );
    }

    pub fn build(self) -> ColorPalette {
        let mut palette = self.palette;
        palette.generation = next_palette_generation();
        palette
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rgb_to_hsla_black() {
        let rgb = Rgb { r: 0, g: 0, b: 0 };
        let hsla = rgb_to_hsla(rgb);
        assert_eq!(hsla.l, 0.0);
        assert_eq!(hsla.s, 0.0);
        assert_eq!(hsla.a, 1.0);
    }

    #[test]
    fn test_rgb_to_hsla_white() {
        let rgb = Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        let hsla = rgb_to_hsla(rgb);
        assert_eq!(hsla.l, 1.0);
        assert_eq!(hsla.s, 0.0);
        assert_eq!(hsla.a, 1.0);
    }

    #[test]
    fn test_rgb_to_hsla_red() {
        let rgb = Rgb { r: 255, g: 0, b: 0 };
        let hsla = rgb_to_hsla(rgb);
        assert_eq!(hsla.h, 0.0);
        assert_eq!(hsla.s, 1.0);
        assert_eq!(hsla.a, 1.0);
    }

    #[test]
    fn test_color_palette_default() {
        let palette = ColorPalette::default();
        assert_eq!(palette.ansi_colors.len(), 16);
        assert_eq!(palette.extended_colors.len(), 256);
    }

    #[test]
    fn test_pastel_neon_ansi_palette() {
        let palette = ColorPalette::default();
        // Verify a few key pastel neon colors are correct
        // ANSI Red (index 1) should be Neon Coral (#FF7E9D)
        let neon_coral = rgb_to_hsla(Rgb {
            r: 0xFF,
            g: 0x7E,
            b: 0x9D,
        });
        assert_eq!(palette.ansi_colors[1].h, neon_coral.h);
        assert_eq!(palette.ansi_colors[1].s, neon_coral.s);
        assert_eq!(palette.ansi_colors[1].l, neon_coral.l);

        // ANSI Blue (index 4) should be Orca Blue (#5E9BFF)
        let orca_blue = rgb_to_hsla(Rgb {
            r: 0x5E,
            g: 0x9B,
            b: 0xFF,
        });
        assert_eq!(palette.ansi_colors[4].h, orca_blue.h);
        assert_eq!(palette.ansi_colors[4].l, orca_blue.l);
    }

    #[test]
    fn test_resolve_named_color() {
        let palette = ColorPalette::new();
        let colors = Colors::default();
        let hsla = palette.resolve(Color::Named(NamedColor::Red), &colors);
        assert!(hsla.a > 0.0);
    }

    #[test]
    fn test_resolve_indexed_color() {
        let palette = ColorPalette::new();
        let colors = Colors::default();
        let hsla = palette.resolve(Color::Indexed(42), &colors);
        assert_eq!(hsla.a, 1.0);
    }

    #[test]
    fn test_resolve_spec_color() {
        let palette = ColorPalette::new();
        let colors = Colors::default();
        let rgb = Rgb {
            r: 128,
            g: 64,
            b: 192,
        };
        let hsla = palette.resolve(Color::Spec(rgb), &colors);
        assert_eq!(hsla.a, 1.0);
    }

    #[test]
    fn test_builder_assigns_fresh_generation() {
        let first = ColorPalette::builder().build();
        let second = ColorPalette::builder().build();

        assert_ne!(first.generation, 0);
        assert_ne!(first.generation, second.generation);
    }
}
