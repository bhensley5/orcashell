//! Semantic zone tracking for OSC 133 shell integration.
//!
//! Tracks the terminal's semantic state (prompt, input, executing, complete)
//! based on OSC 133 markers emitted by shell integration scripts.

use alacritty_terminal::index::Point;
use alacritty_terminal::vte::ansi::SemanticPromptCommand;

/// The current semantic state of the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticState {
    /// No shell integration detected yet.
    Unknown,
    /// Between 133;A and 133;B: the shell is displaying its prompt.
    Prompt,
    /// Between 133;B and 133;C: user is typing a command.
    Input,
    /// Between 133;C and 133;D: a command is executing, output is streaming.
    Executing,
    /// After 133;D: command finished, idle before next prompt.
    CommandComplete { exit_code: Option<i32> },
}

/// Bounds of the current input region on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputRegion {
    /// Where the input begins (cursor position when 133;B was received).
    pub start: Point,
    /// Current end of input (updated as cursor moves during Input state).
    pub end: Point,
}

/// Tracks semantic zones reported by OSC 133 shell integration.
#[derive(Debug, Clone)]
pub struct SemanticZoneTracker {
    state: SemanticState,
    input_region: Option<InputRegion>,
    /// True after PromptStart until the first `update_input_end` call.
    /// The first update captures the cursor position as `input_region.start`.
    /// By that point the shell has finished rendering the prompt, so the
    /// cursor sits at the exact boundary between prompt and user input.
    needs_start_capture: bool,
}

impl SemanticZoneTracker {
    pub fn new() -> Self {
        Self {
            state: SemanticState::Unknown,
            input_region: None,
            needs_start_capture: false,
        }
    }

    pub fn state(&self) -> SemanticState {
        self.state
    }

    pub fn input_region(&self) -> Option<&InputRegion> {
        self.input_region.as_ref()
    }

    /// Whether the terminal is in a state where the user can type commands.
    ///
    /// This is true for both `Prompt` and `Input` states because the user
    /// is typing during the prompt phase. The `B` (CommandStart) marker
    /// only fires when they press Enter, so we can't wait for it.
    pub fn is_inputting(&self) -> bool {
        matches!(self.state, SemanticState::Prompt | SemanticState::Input)
    }

    /// Process an OSC 133 command, updating state and input region.
    pub fn handle_command(&mut self, command: SemanticPromptCommand, cursor_position: Point) {
        match command {
            SemanticPromptCommand::PromptStart => {
                self.state = SemanticState::Prompt;
                // Don't set the input region yet. The prompt text hasn't
                // rendered. The first `update_input_end` call (next render
                // frame) will capture the cursor position as the input start.
                self.input_region = None;
                self.needs_start_capture = true;
            }
            SemanticPromptCommand::CommandStart => {
                self.state = SemanticState::Input;
            }
            SemanticPromptCommand::CommandExecuted => {
                if let Some(ref mut region) = self.input_region {
                    region.end = cursor_position;
                }
                self.state = SemanticState::Executing;
            }
            SemanticPromptCommand::CommandFinished { exit_code } => {
                self.state = SemanticState::CommandComplete { exit_code };
                self.input_region = None;
            }
        }
    }

    /// Update the input region as the cursor moves.
    ///
    /// On the first call after `PromptStart`, this captures the cursor
    /// position as `input_region.start`. The prompt has finished rendering
    /// by this point so the cursor is right where user input begins.
    /// Subsequent calls update `end` to track the rightmost extent of input.
    pub fn update_input_end(&mut self, cursor_position: Point) {
        if !matches!(self.state, SemanticState::Prompt | SemanticState::Input) {
            return;
        }

        if self.needs_start_capture {
            // First render after PromptStart: prompt is done rendering,
            // cursor is at the input start position.
            self.input_region = Some(InputRegion {
                start: cursor_position,
                end: cursor_position,
            });
            self.needs_start_capture = false;
        } else if let Some(ref mut region) = self.input_region {
            region.end = cursor_position;
        }
    }
}

impl Default for SemanticZoneTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
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
}
