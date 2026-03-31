use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};

pub struct TermDimensions {
    columns: usize,
    screen_lines: usize,
}

impl TermDimensions {
    pub fn new(columns: usize, screen_lines: usize) -> Self {
        Self {
            columns,
            screen_lines,
        }
    }
}

impl Dimensions for TermDimensions {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }

    fn last_column(&self) -> Column {
        Column(self.columns.saturating_sub(1))
    }

    fn topmost_line(&self) -> Line {
        Line(0)
    }

    fn bottommost_line(&self) -> Line {
        Line(self.screen_lines as i32 - 1)
    }
}
