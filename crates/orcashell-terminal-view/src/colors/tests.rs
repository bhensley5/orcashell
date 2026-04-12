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
