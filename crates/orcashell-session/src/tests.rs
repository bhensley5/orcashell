use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

use crate::dimensions::TermDimensions;
use crate::engine::feed_bytes_to_term;

fn make_test_term(cols: usize, rows: usize) -> (Term<VoidListener>, Processor) {
    let config = Config::default();
    let dims = TermDimensions::new(cols, rows);
    let term = Term::new(config, &dims, VoidListener);
    let processor = Processor::new();
    (term, processor)
}

#[test]
fn test_term_dimensions() {
    let (term, _) = make_test_term(80, 24);
    assert_eq!(term.grid().columns(), 80);
    assert_eq!(term.grid().screen_lines(), 24);
}

#[test]
fn test_feed_bytes_updates_cells() {
    let (mut term, mut proc) = make_test_term(80, 24);
    feed_bytes_to_term(&mut term, &mut proc, b"hello");

    let grid = term.grid();
    let expected = ['h', 'e', 'l', 'l', 'o'];
    for (i, ch) in expected.iter().enumerate() {
        let cell = &grid[Line(0)][Column(i)];
        assert_eq!(cell.c, *ch, "cell at column {} should be '{}'", i, ch);
    }
}

#[test]
fn test_cursor_movement() {
    let (mut term, mut proc) = make_test_term(80, 24);
    // CSI 6;11 H. Move cursor to row 6, col 11 (1-indexed)
    feed_bytes_to_term(&mut term, &mut proc, b"\x1b[6;11H");

    let cursor = term.grid().cursor.point;
    assert_eq!(cursor.line, Line(5));
    assert_eq!(cursor.column, Column(10));
}

#[test]
fn test_clear_screen() {
    let (mut term, mut proc) = make_test_term(80, 24);
    feed_bytes_to_term(&mut term, &mut proc, b"hello world");
    feed_bytes_to_term(&mut term, &mut proc, b"\x1b[2J");

    let cell = &term.grid()[Line(0)][Column(0)];
    assert!(
        cell.c == ' ' || cell.c == '\0',
        "cell should be blank after clear, got '{}'",
        cell.c
    );
}

#[test]
fn test_resize_term() {
    let (mut term, _) = make_test_term(80, 24);
    let new_dims = TermDimensions::new(120, 40);
    term.resize(new_dims);

    assert_eq!(term.grid().columns(), 120);
    assert_eq!(term.grid().screen_lines(), 40);
}

#[test]
#[ignore] // Requires real PTY. Run with `cargo test -- --ignored`
fn test_pty_session_lifecycle() {
    use crate::engine::SessionEngine;
    use crate::event::{SessionEvent, TerminalColors};
    use std::thread;
    use std::time::Duration;

    let colors = TerminalColors::new((0xd4, 0xd4, 0xd4), (0x1e, 0x1e, 0x1e), (0xff, 0xff, 0xff));
    let mut engine =
        SessionEngine::new(80, 24, 10_000, None, colors).expect("failed to create session");

    thread::sleep(Duration::from_millis(500));

    engine.write(b"exit\n");
    thread::sleep(Duration::from_millis(1000));

    // Drain bytes so ChildExit events can be processed
    engine.process_pending_bytes();

    let mut got_exit = false;
    for _ in 0..100 {
        if let Some(event) = engine.try_recv_event() {
            if matches!(event, SessionEvent::Exit) {
                got_exit = true;
                break;
            }
        }
        thread::sleep(Duration::from_millis(50));
        engine.process_pending_bytes();
    }
    assert!(got_exit, "should receive Exit event after shell exit");
}
