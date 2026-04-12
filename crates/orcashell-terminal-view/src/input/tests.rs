use super::*;

#[test]
fn test_enter_key() {
    let keystroke = Keystroke::parse("enter").unwrap();
    let bytes = keystroke_to_bytes(&keystroke, TermMode::empty());
    assert_eq!(bytes, Some(b"\r".to_vec()));
}

#[test]
fn test_escape_key() {
    let keystroke = Keystroke::parse("escape").unwrap();
    let bytes = keystroke_to_bytes(&keystroke, TermMode::empty());
    assert_eq!(bytes, Some(b"\x1b".to_vec()));
}

#[test]
fn test_backspace_key() {
    let keystroke = Keystroke::parse("backspace").unwrap();
    let bytes = keystroke_to_bytes(&keystroke, TermMode::empty());
    assert_eq!(bytes, Some(b"\x7f".to_vec()));
}

#[test]
fn test_tab_key() {
    let keystroke = Keystroke::parse("tab").unwrap();
    let bytes = keystroke_to_bytes(&keystroke, TermMode::empty());
    assert_eq!(bytes, Some(b"\t".to_vec()));
}

#[test]
fn test_shift_tab() {
    let keystroke = Keystroke::parse("shift-tab").unwrap();
    let bytes = keystroke_to_bytes(&keystroke, TermMode::empty());
    assert_eq!(bytes, Some(b"\x1b[Z".to_vec()));
}

#[test]
fn test_arrow_keys_normal_mode() {
    let mode = TermMode::empty();

    let up = Keystroke::parse("up").unwrap();
    assert_eq!(keystroke_to_bytes(&up, mode), Some(b"\x1b[A".to_vec()));

    let down = Keystroke::parse("down").unwrap();
    assert_eq!(keystroke_to_bytes(&down, mode), Some(b"\x1b[B".to_vec()));

    let right = Keystroke::parse("right").unwrap();
    assert_eq!(keystroke_to_bytes(&right, mode), Some(b"\x1b[C".to_vec()));

    let left = Keystroke::parse("left").unwrap();
    assert_eq!(keystroke_to_bytes(&left, mode), Some(b"\x1b[D".to_vec()));
}

#[test]
fn test_arrow_keys_app_cursor_mode() {
    let mode = TermMode::APP_CURSOR;

    let up = Keystroke::parse("up").unwrap();
    assert_eq!(keystroke_to_bytes(&up, mode), Some(b"\x1bOA".to_vec()));

    let down = Keystroke::parse("down").unwrap();
    assert_eq!(keystroke_to_bytes(&down, mode), Some(b"\x1bOB".to_vec()));

    let right = Keystroke::parse("right").unwrap();
    assert_eq!(keystroke_to_bytes(&right, mode), Some(b"\x1bOC".to_vec()));

    let left = Keystroke::parse("left").unwrap();
    assert_eq!(keystroke_to_bytes(&left, mode), Some(b"\x1bOD".to_vec()));
}

#[test]
fn test_navigation_keys() {
    let mode = TermMode::empty();

    let home = Keystroke::parse("home").unwrap();
    assert_eq!(keystroke_to_bytes(&home, mode), Some(b"\x1b[H".to_vec()));

    let end = Keystroke::parse("end").unwrap();
    assert_eq!(keystroke_to_bytes(&end, mode), Some(b"\x1b[F".to_vec()));

    let pageup = Keystroke::parse("pageup").unwrap();
    assert_eq!(keystroke_to_bytes(&pageup, mode), Some(b"\x1b[5~".to_vec()));

    let pagedown = Keystroke::parse("pagedown").unwrap();
    assert_eq!(
        keystroke_to_bytes(&pagedown, mode),
        Some(b"\x1b[6~".to_vec())
    );

    let insert = Keystroke::parse("insert").unwrap();
    assert_eq!(keystroke_to_bytes(&insert, mode), Some(b"\x1b[2~".to_vec()));

    let delete = Keystroke::parse("delete").unwrap();
    assert_eq!(keystroke_to_bytes(&delete, mode), Some(b"\x1b[3~".to_vec()));
}

#[test]
fn test_function_keys() {
    let mode = TermMode::empty();

    let f1 = Keystroke::parse("f1").unwrap();
    assert_eq!(keystroke_to_bytes(&f1, mode), Some(b"\x1bOP".to_vec()));

    let f2 = Keystroke::parse("f2").unwrap();
    assert_eq!(keystroke_to_bytes(&f2, mode), Some(b"\x1bOQ".to_vec()));

    let f5 = Keystroke::parse("f5").unwrap();
    assert_eq!(keystroke_to_bytes(&f5, mode), Some(b"\x1b[15~".to_vec()));

    let f12 = Keystroke::parse("f12").unwrap();
    assert_eq!(keystroke_to_bytes(&f12, mode), Some(b"\x1b[24~".to_vec()));
}

#[test]
fn test_legacy_modified_arrows() {
    let mode = TermMode::empty();

    // Shift+Up → xterm modified form \x1b[1;2A
    let shift_up = Keystroke::parse("shift-up").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_up, mode),
        Some(b"\x1b[1;2A".to_vec())
    );

    // Ctrl+Right → \x1b[1;5C
    let ctrl_right = Keystroke::parse("ctrl-right").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_right, mode),
        Some(b"\x1b[1;5C".to_vec())
    );

    // On macOS, Option+Left/Right follows Terminal-style word motion.
    // On Linux/Windows, Alt+Arrow keeps xterm modified-arrow behavior.
    let alt_left = Keystroke::parse("alt-left").unwrap();
    let alt_right = Keystroke::parse("alt-right").unwrap();

    if cfg!(target_os = "macos") {
        assert_eq!(keystroke_to_bytes(&alt_left, mode), Some(b"\x1bb".to_vec()));
        assert_eq!(
            keystroke_to_bytes(&alt_right, mode),
            Some(b"\x1bf".to_vec())
        );
    } else {
        assert_eq!(
            keystroke_to_bytes(&alt_left, mode),
            Some(b"\x1b[1;3D".to_vec())
        );
        assert_eq!(
            keystroke_to_bytes(&alt_right, mode),
            Some(b"\x1b[1;3C".to_vec())
        );
    }

    // Unmodified arrows still work normally
    let up = Keystroke::parse("up").unwrap();
    assert_eq!(keystroke_to_bytes(&up, mode), Some(b"\x1b[A".to_vec()));

    // Unmodified + APP_CURSOR still works
    let app_mode = TermMode::APP_CURSOR;
    assert_eq!(keystroke_to_bytes(&up, app_mode), Some(b"\x1bOA".to_vec()));
}

#[test]
fn test_legacy_modified_nav_and_fkeys() {
    let mode = TermMode::empty();

    // Shift+Home → \x1b[1;2H
    let shift_home = Keystroke::parse("shift-home").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_home, mode),
        Some(b"\x1b[1;2H".to_vec())
    );

    // Ctrl+Delete → \x1b[3;5~
    let ctrl_delete = Keystroke::parse("ctrl-delete").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_delete, mode),
        Some(b"\x1b[3;5~".to_vec())
    );

    // Shift+F5 → \x1b[15;2~
    let shift_f5 = Keystroke::parse("shift-f5").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_f5, mode),
        Some(b"\x1b[15;2~".to_vec())
    );

    // Shift+F1 → \x1b[1;2P
    let shift_f1 = Keystroke::parse("shift-f1").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_f1, mode),
        Some(b"\x1b[1;2P".to_vec())
    );
}

#[test]
fn test_ctrl_combinations() {
    let mode = TermMode::empty();

    // Ctrl+A = 0x01
    let ctrl_a = Keystroke::parse("ctrl-a").unwrap();
    assert_eq!(keystroke_to_bytes(&ctrl_a, mode), Some(vec![0x01]));

    // Ctrl+C = 0x03
    let ctrl_c = Keystroke::parse("ctrl-c").unwrap();
    assert_eq!(keystroke_to_bytes(&ctrl_c, mode), Some(vec![0x03]));

    // Ctrl+Z = 0x1a
    let ctrl_z = Keystroke::parse("ctrl-z").unwrap();
    assert_eq!(keystroke_to_bytes(&ctrl_z, mode), Some(vec![0x1a]));

    // Ctrl+Space = 0x00
    let ctrl_space = Keystroke::parse("ctrl-space").unwrap();
    assert_eq!(keystroke_to_bytes(&ctrl_space, mode), Some(vec![0x00]));
}

#[test]
fn test_alt_combinations() {
    let mode = TermMode::empty();

    // Alt+a sends ESC followed by 'a'
    let alt_a = Keystroke::parse("alt-a").unwrap();
    assert_eq!(keystroke_to_bytes(&alt_a, mode), Some(b"\x1ba".to_vec()));

    // Alt+x sends ESC followed by 'x'
    let alt_x = Keystroke::parse("alt-x").unwrap();
    assert_eq!(keystroke_to_bytes(&alt_x, mode), Some(b"\x1bx".to_vec()));

    // Alt+Shift+a sends ESC followed by 'A' (uppercase)
    let alt_shift_a = Keystroke::parse("alt-shift-a").unwrap();
    assert_eq!(
        keystroke_to_bytes(&alt_shift_a, mode),
        Some(b"\x1bA".to_vec())
    );
}

#[test]
fn test_regular_characters() {
    let mode = TermMode::empty();

    let a = Keystroke::parse("a").unwrap();
    assert_eq!(keystroke_to_bytes(&a, mode), Some(b"a".to_vec()));

    let z = Keystroke::parse("z").unwrap();
    assert_eq!(keystroke_to_bytes(&z, mode), Some(b"z".to_vec()));

    let zero = Keystroke::parse("0").unwrap();
    assert_eq!(keystroke_to_bytes(&zero, mode), Some(b"0".to_vec()));
}

#[test]
fn test_space_key() {
    let mode = TermMode::empty();

    let space = Keystroke::parse("space").unwrap();
    assert_eq!(keystroke_to_bytes(&space, mode), Some(b" ".to_vec()));
}

#[test]
fn test_bracketed_paste_enabled() {
    let mode = TermMode::BRACKETED_PASTE;
    let result = wrap_bracketed_paste(b"hello\nworld", mode);
    assert_eq!(result, b"\x1b[200~hello\nworld\x1b[201~");
}

#[test]
fn test_bracketed_paste_disabled() {
    let mode = TermMode::empty();
    let result = wrap_bracketed_paste(b"hello\nworld", mode);
    assert_eq!(result, b"hello\nworld");
}

#[test]
fn test_bracketed_paste_empty_data() {
    let mode = TermMode::BRACKETED_PASTE;
    let result = wrap_bracketed_paste(b"", mode);
    assert_eq!(result, b"\x1b[200~\x1b[201~");
}

// ── Kitty keyboard protocol tests ──────────────────────────────────

#[test]
fn test_kitty_modifier_mask() {
    use gpui::Modifiers;

    let none = Modifiers::default();
    assert_eq!(kitty_modifier_mask(&none), 0);

    let shift = Modifiers {
        shift: true,
        ..Default::default()
    };
    assert_eq!(kitty_modifier_mask(&shift), 1);

    let alt = Modifiers {
        alt: true,
        ..Default::default()
    };
    assert_eq!(kitty_modifier_mask(&alt), 2);

    let ctrl = Modifiers {
        control: true,
        ..Default::default()
    };
    assert_eq!(kitty_modifier_mask(&ctrl), 4);

    let super_mod = Modifiers {
        platform: true,
        ..Default::default()
    };
    assert_eq!(kitty_modifier_mask(&super_mod), 8);

    let ctrl_shift = Modifiers {
        control: true,
        shift: true,
        ..Default::default()
    };
    assert_eq!(kitty_modifier_mask(&ctrl_shift), 5);

    let ctrl_alt_shift = Modifiers {
        control: true,
        alt: true,
        shift: true,
        ..Default::default()
    };
    assert_eq!(kitty_modifier_mask(&ctrl_alt_shift), 7);
}

#[test]
fn test_kitty_special_keys_disambiguate() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES;

    // Unmodified special keys fall through to legacy in level 1
    let enter = Keystroke::parse("enter").unwrap();
    assert_eq!(keystroke_to_bytes(&enter, mode), Some(b"\r".to_vec()));

    let esc = Keystroke::parse("escape").unwrap();
    assert_eq!(keystroke_to_bytes(&esc, mode), Some(b"\x1b".to_vec()));

    let tab = Keystroke::parse("tab").unwrap();
    assert_eq!(keystroke_to_bytes(&tab, mode), Some(b"\t".to_vec()));

    let bs = Keystroke::parse("backspace").unwrap();
    assert_eq!(keystroke_to_bytes(&bs, mode), Some(b"\x7f".to_vec()));

    let space = Keystroke::parse("space").unwrap();
    assert_eq!(keystroke_to_bytes(&space, mode), Some(b" ".to_vec()));

    // Modified special keys use CSI-u
    let shift_enter = Keystroke::parse("shift-enter").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_enter, mode),
        Some(b"\x1b[13;2u".to_vec())
    );

    let ctrl_enter = Keystroke::parse("ctrl-enter").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_enter, mode),
        Some(b"\x1b[13;5u".to_vec())
    );

    let shift_tab = Keystroke::parse("shift-tab").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_tab, mode),
        Some(b"\x1b[9;2u".to_vec())
    );

    let ctrl_space = Keystroke::parse("ctrl-space").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_space, mode),
        Some(b"\x1b[32;5u".to_vec())
    );

    let shift_backspace = Keystroke::parse("shift-backspace").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_backspace, mode),
        Some(b"\x1b[127;2u".to_vec())
    );
}

#[test]
fn test_kitty_special_keys_all_keys() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC;

    // In level 4, even unmodified special keys use CSI-u
    let enter = Keystroke::parse("enter").unwrap();
    assert_eq!(keystroke_to_bytes(&enter, mode), Some(b"\x1b[13u".to_vec()));

    let tab = Keystroke::parse("tab").unwrap();
    assert_eq!(keystroke_to_bytes(&tab, mode), Some(b"\x1b[9u".to_vec()));

    let esc = Keystroke::parse("escape").unwrap();
    assert_eq!(keystroke_to_bytes(&esc, mode), Some(b"\x1b[27u".to_vec()));

    let space = Keystroke::parse("space").unwrap();
    assert_eq!(keystroke_to_bytes(&space, mode), Some(b"\x1b[32u".to_vec()));

    let bs = Keystroke::parse("backspace").unwrap();
    assert_eq!(keystroke_to_bytes(&bs, mode), Some(b"\x1b[127u".to_vec()));

    // Modified special keys in level 4
    let shift_enter = Keystroke::parse("shift-enter").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_enter, mode),
        Some(b"\x1b[13;2u".to_vec())
    );
}

#[test]
fn test_kitty_arrows_with_modifiers() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES;

    let shift_up = Keystroke::parse("shift-up").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_up, mode),
        Some(b"\x1b[1;2A".to_vec())
    );

    let ctrl_down = Keystroke::parse("ctrl-down").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_down, mode),
        Some(b"\x1b[1;5B".to_vec())
    );

    let ctrl_right = Keystroke::parse("ctrl-right").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_right, mode),
        Some(b"\x1b[1;5C".to_vec())
    );

    let alt_left = Keystroke::parse("alt-left").unwrap();
    assert_eq!(
        keystroke_to_bytes(&alt_left, mode),
        Some(b"\x1b[1;3D".to_vec())
    );
}

#[test]
fn test_kitty_arrows_unmodified_legacy() {
    // Unmodified arrows in Kitty mode fall through to legacy
    let mode = TermMode::DISAMBIGUATE_ESC_CODES;
    let up = Keystroke::parse("up").unwrap();
    assert_eq!(keystroke_to_bytes(&up, mode), Some(b"\x1b[A".to_vec()));

    // APP_CURSOR still applies for unmodified arrows
    let mode_app = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::APP_CURSOR;
    assert_eq!(keystroke_to_bytes(&up, mode_app), Some(b"\x1bOA".to_vec()));
}

#[test]
fn test_kitty_nav_keys_with_modifiers() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES;

    let shift_home = Keystroke::parse("shift-home").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_home, mode),
        Some(b"\x1b[1;2H".to_vec())
    );

    let ctrl_end = Keystroke::parse("ctrl-end").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_end, mode),
        Some(b"\x1b[1;5F".to_vec())
    );

    let ctrl_pageup = Keystroke::parse("ctrl-pageup").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_pageup, mode),
        Some(b"\x1b[5;5~".to_vec())
    );

    let shift_pagedown = Keystroke::parse("shift-pagedown").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_pagedown, mode),
        Some(b"\x1b[6;2~".to_vec())
    );

    let ctrl_delete = Keystroke::parse("ctrl-delete").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_delete, mode),
        Some(b"\x1b[3;5~".to_vec())
    );

    let shift_insert = Keystroke::parse("shift-insert").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_insert, mode),
        Some(b"\x1b[2;2~".to_vec())
    );
}

#[test]
fn test_kitty_function_keys_with_modifiers() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES;

    // F1-F4: SS3 → CSI with modifier
    let shift_f1 = Keystroke::parse("shift-f1").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_f1, mode),
        Some(b"\x1b[1;2P".to_vec())
    );

    let ctrl_f4 = Keystroke::parse("ctrl-f4").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_f4, mode),
        Some(b"\x1b[1;5S".to_vec())
    );

    // F5-F12: tilde format with modifier
    let ctrl_f5 = Keystroke::parse("ctrl-f5").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_f5, mode),
        Some(b"\x1b[15;5~".to_vec())
    );

    let shift_f12 = Keystroke::parse("shift-f12").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_f12, mode),
        Some(b"\x1b[24;2~".to_vec())
    );
}

#[test]
fn test_kitty_regular_chars_disambiguate() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES;

    // Unmodified chars in level 1 use legacy
    let a = Keystroke::parse("a").unwrap();
    assert_eq!(keystroke_to_bytes(&a, mode), Some(b"a".to_vec()));

    // Ctrl+letter uses CSI-u (not raw control char)
    let ctrl_c = Keystroke::parse("ctrl-c").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_c, mode),
        Some(b"\x1b[99;5u".to_vec())
    );

    let ctrl_a = Keystroke::parse("ctrl-a").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_a, mode),
        Some(b"\x1b[97;5u".to_vec())
    );

    // Alt+letter uses CSI-u (not ESC prefix)
    let alt_a = Keystroke::parse("alt-a").unwrap();
    assert_eq!(
        keystroke_to_bytes(&alt_a, mode),
        Some(b"\x1b[97;3u".to_vec())
    );

    // Shift+letter in disambiguate mode is unambiguous. Falls through to
    // legacy encoding which produces the uppercase character, not CSI-u.
    let shift_a = Keystroke::parse("shift-a").unwrap();
    assert_eq!(keystroke_to_bytes(&shift_a, mode), Some(b"A".to_vec()));

    // Ctrl+Shift still needs CSI-u for disambiguation
    let ctrl_shift_a = Keystroke::parse("ctrl-shift-a").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_shift_a, mode),
        Some(b"\x1b[97;6u".to_vec())
    );
}

#[test]
fn test_kitty_regular_chars_all_keys() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC;

    // In level 4, even unmodified chars get CSI-u
    let a = Keystroke::parse("a").unwrap();
    assert_eq!(keystroke_to_bytes(&a, mode), Some(b"\x1b[97u".to_vec()));

    let one = Keystroke::parse("1").unwrap();
    assert_eq!(keystroke_to_bytes(&one, mode), Some(b"\x1b[49u".to_vec()));

    let zero = Keystroke::parse("0").unwrap();
    assert_eq!(keystroke_to_bytes(&zero, mode), Some(b"\x1b[48u".to_vec()));

    // Modified chars in level 4
    let ctrl_c = Keystroke::parse("ctrl-c").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_c, mode),
        Some(b"\x1b[99;5u".to_vec())
    );
}

#[test]
fn test_kitty_all_keys_without_disambiguate() {
    // REPORT_ALL_KEYS_AS_ESC alone (without DISAMBIGUATE) should still encode
    let mode = TermMode::REPORT_ALL_KEYS_AS_ESC;

    let a = Keystroke::parse("a").unwrap();
    assert_eq!(keystroke_to_bytes(&a, mode), Some(b"\x1b[97u".to_vec()));

    let enter = Keystroke::parse("enter").unwrap();
    assert_eq!(keystroke_to_bytes(&enter, mode), Some(b"\x1b[13u".to_vec()));

    let ctrl_c = Keystroke::parse("ctrl-c").unwrap();
    assert_eq!(
        keystroke_to_bytes(&ctrl_c, mode),
        Some(b"\x1b[99;5u".to_vec())
    );
}

#[test]
fn test_kitty_app_cursor_interaction() {
    // Kitty + APP_CURSOR + no mods → APP_CURSOR applies (SS3)
    let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::APP_CURSOR;
    let up = Keystroke::parse("up").unwrap();
    assert_eq!(keystroke_to_bytes(&up, mode), Some(b"\x1bOA".to_vec()));

    // Kitty + APP_CURSOR + Shift → Kitty modifier format overrides APP_CURSOR
    let shift_up = Keystroke::parse("shift-up").unwrap();
    assert_eq!(
        keystroke_to_bytes(&shift_up, mode),
        Some(b"\x1b[1;2A".to_vec())
    );
}

// -----------------------------------------------------------------------
// REPORT_EVENT_TYPES tests
// -----------------------------------------------------------------------

#[test]
fn test_event_type_text_key_stays_utf8() {
    // DISAMBIGUATE + EVENT_TYPES: text keys stay plain UTF-8.
    let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES;
    let ks = Keystroke::parse("a").unwrap();

    // Press → plain 'a'
    let press = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Press,
        associated_text: None,
    };
    assert_eq!(key_input_to_bytes(&press, mode), Some(b"a".to_vec()));

    // Repeat → still plain 'a' (re-sends the byte)
    let repeat = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Repeat,
        associated_text: None,
    };
    assert_eq!(key_input_to_bytes(&repeat, mode), Some(b"a".to_vec()));

    // Release → None (can't annotate plain UTF-8)
    let release = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Release,
        associated_text: None,
    };
    assert_eq!(key_input_to_bytes(&release, mode), None);
}

#[test]
fn test_event_type_text_key_with_all_keys() {
    // ALL_KEYS + EVENT_TYPES: text keys get CSI-u with event annotations.
    let mode = TermMode::DISAMBIGUATE_ESC_CODES
        | TermMode::REPORT_EVENT_TYPES
        | TermMode::REPORT_ALL_KEYS_AS_ESC;
    let ks = Keystroke::parse("a").unwrap();

    // Press → CSI-u (event_type omitted for press)
    let press = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Press,
        associated_text: None,
    };
    assert_eq!(key_input_to_bytes(&press, mode), Some(b"\x1b[97u".to_vec()));

    // Repeat → CSI-u with :2
    let repeat = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Repeat,
        associated_text: None,
    };
    assert_eq!(
        key_input_to_bytes(&repeat, mode),
        Some(b"\x1b[97;1:2u".to_vec())
    );

    // Release → CSI-u with :3
    let release = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Release,
        associated_text: None,
    };
    assert_eq!(
        key_input_to_bytes(&release, mode),
        Some(b"\x1b[97;1:3u".to_vec())
    );
}

#[test]
fn test_event_type_escape_encoded_key() {
    // Ctrl+a is escape-encoded in DISAMBIGUATE mode. Gets event annotations.
    let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES;
    let ks = Keystroke::parse("ctrl-c").unwrap();

    // Press → CSI-u (no event annotation)
    let press = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Press,
        associated_text: None,
    };
    assert_eq!(
        key_input_to_bytes(&press, mode),
        Some(b"\x1b[99;5u".to_vec())
    );

    // Repeat → CSI-u with :2
    let repeat = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Repeat,
        associated_text: None,
    };
    assert_eq!(
        key_input_to_bytes(&repeat, mode),
        Some(b"\x1b[99;5:2u".to_vec())
    );

    // Release → CSI-u with :3
    let release = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Release,
        associated_text: None,
    };
    assert_eq!(
        key_input_to_bytes(&release, mode),
        Some(b"\x1b[99;5:3u".to_vec())
    );
}

#[test]
fn test_event_type_functional_key() {
    // Functional keys (arrows) are CSI sequences. Get event annotations
    // even without keyboard modifiers.
    let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES;
    let ks = Keystroke::parse("up").unwrap();

    // Press → legacy \x1b[A (no annotation needed)
    let press = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Press,
        associated_text: None,
    };
    assert_eq!(key_input_to_bytes(&press, mode), Some(b"\x1b[A".to_vec()));

    // Repeat → modified form with :2
    let repeat = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Repeat,
        associated_text: None,
    };
    assert_eq!(
        key_input_to_bytes(&repeat, mode),
        Some(b"\x1b[1;1:2A".to_vec())
    );

    // Release → modified form with :3
    let release = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Release,
        associated_text: None,
    };
    assert_eq!(
        key_input_to_bytes(&release, mode),
        Some(b"\x1b[1;1:3A".to_vec())
    );
}

#[test]
fn test_event_type_functional_with_mods() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES;
    let ks = Keystroke::parse("shift-up").unwrap();

    // Shift+Up release → \x1b[1;2:3A
    let release = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Release,
        associated_text: None,
    };
    assert_eq!(
        key_input_to_bytes(&release, mode),
        Some(b"\x1b[1;2:3A".to_vec())
    );
}

#[test]
fn test_event_type_tilde_key() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES;
    let ks = Keystroke::parse("pageup").unwrap();

    // PageUp release → \x1b[5;1:3~
    let release = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Release,
        associated_text: None,
    };
    assert_eq!(
        key_input_to_bytes(&release, mode),
        Some(b"\x1b[5;1:3~".to_vec())
    );
}

#[test]
fn test_event_type_ignored_without_flag() {
    // Without REPORT_EVENT_TYPES, Repeat is treated as Press and Release → None.
    let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC;
    let ks = Keystroke::parse("a").unwrap();

    let repeat = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Repeat,
        associated_text: None,
    };
    // Treated as press → \x1b[97u
    assert_eq!(
        key_input_to_bytes(&repeat, mode),
        Some(b"\x1b[97u".to_vec())
    );

    let release = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Release,
        associated_text: None,
    };
    assert_eq!(key_input_to_bytes(&release, mode), None);
}

#[test]
fn test_event_type_special_key_enter() {
    // Enter in DISAMBIGUATE + EVENT_TYPES (without ALL_KEYS): stays legacy \r.
    let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES;
    let ks = Keystroke::parse("enter").unwrap();

    let repeat = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Repeat,
        associated_text: None,
    };
    assert_eq!(key_input_to_bytes(&repeat, mode), Some(b"\r".to_vec()));

    let release = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Release,
        associated_text: None,
    };
    assert_eq!(key_input_to_bytes(&release, mode), None);

    // With ALL_KEYS: Enter gets CSI-u with event annotations.
    let mode_all = mode | TermMode::REPORT_ALL_KEYS_AS_ESC;
    let repeat_all = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Repeat,
        associated_text: None,
    };
    assert_eq!(
        key_input_to_bytes(&repeat_all, mode_all),
        Some(b"\x1b[13;1:2u".to_vec())
    );

    let release_all = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Release,
        associated_text: None,
    };
    assert_eq!(
        key_input_to_bytes(&release_all, mode_all),
        Some(b"\x1b[13;1:3u".to_vec())
    );
}

// -----------------------------------------------------------------------
// REPORT_ASSOCIATED_TEXT tests
// -----------------------------------------------------------------------

#[test]
fn test_associated_text_simple() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES
        | TermMode::REPORT_ALL_KEYS_AS_ESC
        | TermMode::REPORT_ASSOCIATED_TEXT;
    let ks = Keystroke::parse("a").unwrap();

    let input = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Press,
        associated_text: Some("a"),
    };
    // \x1b[97;;97u. Empty modifier param (mods=0 but text forces it), text=97
    assert_eq!(
        key_input_to_bytes(&input, mode),
        Some(b"\x1b[97;;97u".to_vec())
    );
}

#[test]
fn test_associated_text_with_mods() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES
        | TermMode::REPORT_ALL_KEYS_AS_ESC
        | TermMode::REPORT_ASSOCIATED_TEXT;
    let ks = Keystroke::parse("shift-a").unwrap();

    let input = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Press,
        associated_text: Some("A"),
    };
    // \x1b[97;2;65u. shift=1→mods+1=2, text='A'=65
    assert_eq!(
        key_input_to_bytes(&input, mode),
        Some(b"\x1b[97;2;65u".to_vec())
    );
}

#[test]
fn test_associated_text_unicode() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES
        | TermMode::REPORT_ALL_KEYS_AS_ESC
        | TermMode::REPORT_ASSOCIATED_TEXT;
    let ks = Keystroke::parse("a").unwrap();

    // Simulate a layout producing 'ä' (U+00E4 = 228)
    let input = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Press,
        associated_text: Some("ä"),
    };
    assert_eq!(
        key_input_to_bytes(&input, mode),
        Some(b"\x1b[97;;228u".to_vec())
    );
}

#[test]
fn test_associated_text_ignored_without_flag() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC;
    // No REPORT_ASSOCIATED_TEXT
    let ks = Keystroke::parse("a").unwrap();

    let input = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Press,
        associated_text: Some("a"),
    };
    // Text should NOT be included
    assert_eq!(key_input_to_bytes(&input, mode), Some(b"\x1b[97u".to_vec()));
}

#[test]
fn test_associated_text_not_on_release() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES
        | TermMode::REPORT_ALL_KEYS_AS_ESC
        | TermMode::REPORT_EVENT_TYPES
        | TermMode::REPORT_ASSOCIATED_TEXT;
    let ks = Keystroke::parse("a").unwrap();

    let release = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Release,
        associated_text: Some("a"),
    };
    // Release events never include text
    assert_eq!(
        key_input_to_bytes(&release, mode),
        Some(b"\x1b[97;1:3u".to_vec())
    );
}

// -----------------------------------------------------------------------
// Unicode CSI-u tests
// -----------------------------------------------------------------------

#[test]
fn test_unicode_csi_u_encoding() {
    // encode_csi_u_full directly with non-ASCII codepoint
    let bytes = encode_csi_u_full(0x00F1, 0, None, None); // ñ = 241
    assert_eq!(bytes, b"\x1b[241u".to_vec());

    let bytes = encode_csi_u_full(0x00E4, 2, None, None); // ä with shift
    assert_eq!(bytes, b"\x1b[228;3u".to_vec());
}

// -----------------------------------------------------------------------
// Modifier key encoding tests
// -----------------------------------------------------------------------

#[test]
fn test_modifier_key_press() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES
        | TermMode::REPORT_EVENT_TYPES
        | TermMode::REPORT_ALL_KEYS_AS_ESC;

    // Shift press: codepoint=57441, mods=shift(1)→mods+1=2, press omitted
    let bytes = modifier_key_to_bytes(KITTY_LEFT_SHIFT, 1, KeyEventType::Press, mode);
    assert_eq!(bytes, Some(b"\x1b[57441;2u".to_vec()));
}

#[test]
fn test_modifier_key_release() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES
        | TermMode::REPORT_EVENT_TYPES
        | TermMode::REPORT_ALL_KEYS_AS_ESC;

    // Shift release: after releasing, mods=0 → mods+1=1, event_type=3
    let bytes = modifier_key_to_bytes(KITTY_LEFT_SHIFT, 0, KeyEventType::Release, mode);
    assert_eq!(bytes, Some(b"\x1b[57441;1:3u".to_vec()));
}

#[test]
fn test_modifier_key_without_event_types() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC;
    // Without REPORT_EVENT_TYPES, modifier_key_to_bytes returns None
    let bytes = modifier_key_to_bytes(KITTY_LEFT_SHIFT, 1, KeyEventType::Press, mode);
    assert_eq!(bytes, None);
}

// -----------------------------------------------------------------------
// Combined modes tests
// -----------------------------------------------------------------------

#[test]
fn test_all_modes_combined() {
    let mode = TermMode::DISAMBIGUATE_ESC_CODES
        | TermMode::REPORT_EVENT_TYPES
        | TermMode::REPORT_ALL_KEYS_AS_ESC
        | TermMode::REPORT_ASSOCIATED_TEXT;
    let ks = Keystroke::parse("a").unwrap();

    // Press with text
    let press = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Press,
        associated_text: Some("a"),
    };
    assert_eq!(
        key_input_to_bytes(&press, mode),
        Some(b"\x1b[97;;97u".to_vec())
    );

    // Repeat with text
    let repeat = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Repeat,
        associated_text: Some("a"),
    };
    assert_eq!(
        key_input_to_bytes(&repeat, mode),
        Some(b"\x1b[97;1:2;97u".to_vec())
    );

    // Release. No text
    let release = KeyInput {
        keystroke: &ks,
        event_type: KeyEventType::Release,
        associated_text: Some("a"),
    };
    assert_eq!(
        key_input_to_bytes(&release, mode),
        Some(b"\x1b[97;1:3u".to_vec())
    );
}
