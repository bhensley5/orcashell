use super::*;
use alacritty_terminal::index::{Column, Line};

fn pt(line: i32, col: usize) -> Point {
    Point::new(Line(line), Column(col))
}

#[test]
fn initial_state() {
    let tracker = SemanticZoneTracker::new();
    assert_eq!(tracker.state(), SemanticState::Unknown);
    assert!(tracker.input_region().is_none());
    assert!(!tracker.is_inputting());
}

#[test]
fn prompt_start_defers_input_region() {
    let mut tracker = SemanticZoneTracker::new();
    tracker.handle_command(SemanticPromptCommand::PromptStart, pt(0, 0));
    assert_eq!(tracker.state(), SemanticState::Prompt);
    assert!(tracker.is_inputting());
    // Input region is NOT set yet. Waiting for first render.
    assert!(tracker.input_region().is_none());
}

#[test]
fn first_update_captures_input_start() {
    let mut tracker = SemanticZoneTracker::new();
    // A: prompt starts at col 0.
    tracker.handle_command(SemanticPromptCommand::PromptStart, pt(0, 0));
    assert!(tracker.input_region().is_none());

    // First render: prompt has rendered "$ ", cursor at col 2.
    // This captures col 2 as the input start.
    tracker.update_input_end(pt(0, 2));
    let region = tracker.input_region().unwrap();
    assert_eq!(region.start, pt(0, 2));
    assert_eq!(region.end, pt(0, 2));

    // User types "hello". Cursor moves to col 7.
    // Start stays at col 2, end moves to col 7.
    tracker.update_input_end(pt(0, 7));
    let region = tracker.input_region().unwrap();
    assert_eq!(region.start, pt(0, 2));
    assert_eq!(region.end, pt(0, 7));
}

#[test]
fn full_command_cycle() {
    let mut tracker = SemanticZoneTracker::new();

    // A: prompt starts
    tracker.handle_command(SemanticPromptCommand::PromptStart, pt(0, 0));
    assert_eq!(tracker.state(), SemanticState::Prompt);
    assert!(tracker.is_inputting());

    // First render captures input start at cursor position.
    tracker.update_input_end(pt(0, 2));
    assert!(tracker.input_region().is_some());

    // User types, cursor at col 10.
    tracker.update_input_end(pt(0, 10));
    let region = tracker.input_region().unwrap();
    assert_eq!(region.start, pt(0, 2));
    assert_eq!(region.end, pt(0, 10));

    // B: user hits Enter
    tracker.handle_command(SemanticPromptCommand::CommandStart, pt(0, 10));
    assert_eq!(tracker.state(), SemanticState::Input);
    assert!(tracker.is_inputting());
    // Input region preserved.
    assert!(tracker.input_region().is_some());

    // C: command executes
    tracker.handle_command(SemanticPromptCommand::CommandExecuted, pt(0, 10));
    assert_eq!(tracker.state(), SemanticState::Executing);
    assert!(!tracker.is_inputting());

    // D: command finishes
    tracker.handle_command(
        SemanticPromptCommand::CommandFinished { exit_code: Some(0) },
        pt(5, 0),
    );
    assert_eq!(
        tracker.state(),
        SemanticState::CommandComplete { exit_code: Some(0) }
    );
    assert!(tracker.input_region().is_none());

    // Next A: starts fresh
    tracker.handle_command(SemanticPromptCommand::PromptStart, pt(5, 0));
    assert_eq!(tracker.state(), SemanticState::Prompt);
    // Deferred. No input region until first render.
    assert!(tracker.input_region().is_none());
}

#[test]
fn update_input_end_after_command_start() {
    let mut tracker = SemanticZoneTracker::new();
    tracker.handle_command(SemanticPromptCommand::PromptStart, pt(0, 0));
    tracker.update_input_end(pt(0, 2)); // capture start
    tracker.handle_command(SemanticPromptCommand::CommandStart, pt(0, 5));

    // Still updates end in Input state.
    tracker.update_input_end(pt(0, 8));
    assert_eq!(tracker.input_region().unwrap().end, pt(0, 8));
}

#[test]
fn update_input_end_ignored_when_executing() {
    let mut tracker = SemanticZoneTracker::new();
    tracker.handle_command(SemanticPromptCommand::PromptStart, pt(0, 0));
    tracker.update_input_end(pt(0, 2)); // capture start
    tracker.handle_command(SemanticPromptCommand::CommandStart, pt(0, 5));
    tracker.handle_command(SemanticPromptCommand::CommandExecuted, pt(0, 5));

    let end_before = tracker.input_region().unwrap().end;
    tracker.update_input_end(pt(3, 0));
    assert_eq!(tracker.input_region().unwrap().end, end_before);
}

#[test]
fn command_finished_without_exit_code() {
    let mut tracker = SemanticZoneTracker::new();
    tracker.handle_command(SemanticPromptCommand::PromptStart, pt(0, 0));
    tracker.update_input_end(pt(0, 2)); // capture start
    tracker.handle_command(SemanticPromptCommand::CommandStart, pt(0, 5));
    tracker.handle_command(SemanticPromptCommand::CommandExecuted, pt(0, 5));
    tracker.handle_command(
        SemanticPromptCommand::CommandFinished { exit_code: None },
        pt(3, 0),
    );
    assert_eq!(
        tracker.state(),
        SemanticState::CommandComplete { exit_code: None }
    );
    assert!(tracker.input_region().is_none());
}
