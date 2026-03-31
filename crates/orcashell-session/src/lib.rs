pub mod cwd;
pub mod dimensions;
pub mod engine;
pub mod error;
pub mod event;
pub mod semantic_zone;
pub mod shell_integration;

pub use engine::{feed_bytes_to_term, SessionEngine};
pub use error::SessionError;
pub use event::SessionEvent;
pub use semantic_zone::{InputRegion, SemanticState, SemanticZoneTracker};
pub use shell_integration::ShellType;

#[cfg(test)]
mod tests;
