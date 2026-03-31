use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::vte::ansi::{Rgb, SemanticPromptCommand};
use parking_lot::Mutex;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use tracing::debug;

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Wakeup,
    Bell,
    Title(String),
    ClipboardStore(String),
    ClipboardLoad,
    Exit,
    /// OSC 133 semantic prompt command.
    SemanticPrompt(SemanticPromptCommand),
    /// OSC 9 or OSC 777 desktop notification.
    Notification {
        title: String,
        body: String,
    },
}

/// Default terminal colors for responding to OSC 10/11/12 queries.
pub struct TerminalColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
}

impl TerminalColors {
    pub fn new(fg: (u8, u8, u8), bg: (u8, u8, u8), cursor: (u8, u8, u8)) -> Self {
        Self {
            foreground: Rgb {
                r: fg.0,
                g: fg.1,
                b: fg.2,
            },
            background: Rgb {
                r: bg.0,
                g: bg.1,
                b: bg.2,
            },
            cursor: Rgb {
                r: cursor.0,
                g: cursor.1,
                b: cursor.2,
            },
        }
    }
}

/// Shared terminal size, updated by the renderer and read by EventProxy.
///
/// Packs `num_lines`, `num_cols`, `cell_width`, `cell_height` (all u16)
/// into a single AtomicU64 for lock-free reads during event handling.
#[derive(Clone)]
pub struct SharedWindowSize(pub Arc<AtomicU64>);

impl SharedWindowSize {
    pub fn new(lines: u16, cols: u16, cell_w: u16, cell_h: u16) -> Self {
        Self(Arc::new(AtomicU64::new(Self::pack(
            lines, cols, cell_w, cell_h,
        ))))
    }

    pub fn update(&self, lines: u16, cols: u16, cell_w: u16, cell_h: u16) {
        self.0
            .store(Self::pack(lines, cols, cell_w, cell_h), Ordering::Relaxed);
    }

    pub fn load(&self) -> WindowSize {
        let v = self.0.load(Ordering::Relaxed);
        WindowSize {
            num_lines: (v >> 48) as u16,
            num_cols: (v >> 32) as u16,
            cell_width: (v >> 16) as u16,
            cell_height: v as u16,
        }
    }

    fn pack(lines: u16, cols: u16, cell_w: u16, cell_h: u16) -> u64 {
        (lines as u64) << 48 | (cols as u64) << 32 | (cell_w as u64) << 16 | cell_h as u64
    }
}

pub struct EventProxy {
    event_tx: Sender<SessionEvent>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    default_colors: TerminalColors,
    window_size: SharedWindowSize,
}

impl EventProxy {
    pub fn new(
        event_tx: Sender<SessionEvent>,
        pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
        window_size: SharedWindowSize,
        colors: TerminalColors,
    ) -> Self {
        Self {
            event_tx,
            pty_writer,
            default_colors: colors,
            window_size,
        }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => {
                let _ = self.event_tx.send(SessionEvent::Wakeup);
            }
            Event::Bell => {
                let _ = self.event_tx.send(SessionEvent::Bell);
            }
            Event::Title(title) => {
                let _ = self.event_tx.send(SessionEvent::Title(title));
            }
            Event::ResetTitle => {
                let _ = self.event_tx.send(SessionEvent::Title(String::new()));
            }
            Event::ClipboardStore(_clipboard_type, data) => {
                let _ = self.event_tx.send(SessionEvent::ClipboardStore(data));
            }
            Event::ClipboardLoad(_clipboard_type, _format) => {
                let _ = self.event_tx.send(SessionEvent::ClipboardLoad);
            }
            Event::PtyWrite(data) => {
                // Write DSR and other responses back to PTY.
                // Called from within Processor::advance() while the Term lock IS held.
                // pty_writer uses a separate parking_lot::Mutex. No deadlock.
                let mut writer = self.pty_writer.lock();
                let _ = writer.write_all(data.as_bytes());
                let _ = writer.flush();
            }
            Event::Exit => {
                let _ = self.event_tx.send(SessionEvent::Exit);
            }
            Event::ChildExit(_code) => {
                let _ = self.event_tx.send(SessionEvent::Exit);
            }
            Event::SemanticPrompt(cmd) => {
                debug!("OSC 133 received: {cmd:?}");
                let _ = self.event_tx.send(SessionEvent::SemanticPrompt(cmd));
            }
            Event::Notification { title, body } => {
                debug!("Notification received: title={title:?}");
                let _ = self
                    .event_tx
                    .send(SessionEvent::Notification { title, body });
            }
            Event::ColorRequest(index, format) => {
                // Respond to OSC 10/11/12 color queries.
                // Index 256 = foreground, 257 = background, 258 = cursor.
                let color = match index {
                    256 => Some(self.default_colors.foreground),
                    257 => Some(self.default_colors.background),
                    258 => Some(self.default_colors.cursor),
                    _ => None,
                };
                if let Some(rgb) = color {
                    let text = format(rgb);
                    let mut writer = self.pty_writer.lock();
                    let _ = writer.write_all(text.as_bytes());
                    let _ = writer.flush();
                }
            }
            Event::TextAreaSizeRequest(format) => {
                let ws = self.window_size.load();
                let text = format(ws);
                let mut writer = self.pty_writer.lock();
                let _ = writer.write_all(text.as_bytes());
                let _ = writer.flush();
            }
            Event::MouseCursorDirty | Event::CursorBlinkingChange => {}
        }
    }
}
