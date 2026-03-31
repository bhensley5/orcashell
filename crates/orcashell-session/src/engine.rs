use crate::dimensions::TermDimensions;
use crate::error::SessionError;
use crate::event::{EventProxy, SessionEvent, SharedWindowSize, TerminalColors};
use crate::semantic_zone::SemanticZoneTracker;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use crossbeam_channel::{self, Receiver, Sender};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use tracing::{debug, warn};

pub struct SessionEngine {
    term: Arc<FairMutex<Term<EventProxy>>>,
    processor: Processor,
    pty_master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Mutex<Option<Box<dyn Child + Send + Sync>>>,
    /// Cached PID from spawn time. Never changes after construction.
    child_pid: Option<u32>,
    shell_type: crate::shell_integration::ShellType,
    byte_rx: Receiver<Vec<u8>>,
    dirty: Arc<AtomicBool>,
    wake_rx: Option<async_channel::Receiver<()>>,
    event_rx: mpsc::Receiver<SessionEvent>,
    zone_tracker: SemanticZoneTracker,
    window_size: SharedWindowSize,
    _reader_handle: JoinHandle<()>,
}

impl SessionEngine {
    pub fn new(
        cols: usize,
        rows: usize,
        scrollback: usize,
        cwd: Option<&std::path::Path>,
        colors: TerminalColors,
    ) -> Result<Self, SessionError> {
        Self::new_with_shell(cols, rows, scrollback, cwd, colors, None)
    }

    pub fn new_with_shell(
        cols: usize,
        rows: usize,
        scrollback: usize,
        cwd: Option<&std::path::Path>,
        colors: TerminalColors,
        shell_override: Option<&str>,
    ) -> Result<Self, SessionError> {
        let cols = cols.min(u16::MAX as usize).max(1);
        let rows = rows.min(u16::MAX as usize).max(1);

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: rows as u16,
                cols: cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(SessionError::PtyCreation)?;

        let shell = crate::shell_integration::resolve_shell_path(shell_override);

        // Prepare shell integration scripts for OSC 133 semantic prompts.
        let integration_dir = crate::shell_integration::prepare_integration_dir()
            .map_err(SessionError::ShellIntegration)?;
        let detected_shell = crate::shell_integration::shell_type(&shell);

        let mut cmd = CommandBuilder::new(&shell);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("ORCASHELL", "1");
        // Don't set TERM_PROGRAM. Claude Code (and other apps) check it
        // before KITTY_WINDOW_ID. If TERM_PROGRAM is set to an unrecognized
        // name, KITTY_WINDOW_ID is never reached. We use ORCASHELL=1 for
        // our own detection instead.
        cmd.env_remove("TERM_PROGRAM");
        // Signal Kitty keyboard protocol support to child processes.
        // Apps like Claude Code check this env var to decide whether
        // to enable the protocol (CSI > 1 u push sequence).
        cmd.env("KITTY_WINDOW_ID", "1");

        // Inject shell integration via shell-specific mechanisms.
        match detected_shell {
            crate::shell_integration::ShellType::Zsh => {
                // Preserve the user's real dotdir so our wrapper startup files
                // can source normal interactive config before installing hooks.
                if let Some(real_zdotdir) = std::env::var_os("ZDOTDIR") {
                    cmd.env("ORCASHELL_REAL_ZDOTDIR", real_zdotdir);
                }
                // ZDOTDIR makes zsh read our proxy startup files first.
                if let Some(dir_str) = integration_dir.to_str() {
                    cmd.env("ZDOTDIR", dir_str);
                }
                // Login shells load ~/.zprofile and related startup files that
                // Finder-launched apps would otherwise miss on macOS.
                cmd.arg("-l");
            }
            crate::shell_integration::ShellType::Bash => {
                // Bash login shells read ~/.bash_profile (or fallback files)
                // rather than ~/.bashrc, so point HOME at our wrapper dir and
                // restore the user's real HOME inside the wrapper.
                if let Some(real_home) = std::env::var_os("HOME") {
                    cmd.env("ORCASHELL_REAL_HOME", real_home);
                }
                if let Some(dir_str) = integration_dir.to_str() {
                    cmd.env("HOME", dir_str);
                }
                cmd.arg("--login");
            }
            crate::shell_integration::ShellType::PowerShellCore
            | crate::shell_integration::ShellType::WindowsPowerShell => {
                cmd.arg("-NoLogo");
                cmd.arg("-NoExit");
                cmd.arg("-File");
                cmd.arg(integration_dir.join("orcashell.ps1"));
            }
            crate::shell_integration::ShellType::Cmd => {
                debug!("CMD shell. Launching without OSC 133 integration");
            }
            crate::shell_integration::ShellType::Unknown => {
                debug!("Unknown shell type '{}', skipping shell integration", shell);
            }
        }

        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| SessionError::ShellSpawn {
                shell: shell.clone(),
                source: e,
            })?;

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(SessionError::PtyReader)?;

        let writer = pair.master.take_writer().map_err(SessionError::PtyWriter)?;

        let pty_writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(Box::new(writer)));

        let (event_tx, event_rx) = mpsc::channel();
        // Initial cell dimensions unknown (0); renderer updates after first measure.
        let window_size = SharedWindowSize::new(rows as u16, cols as u16, 0, 0);
        let event_proxy = EventProxy::new(
            event_tx,
            Arc::clone(&pty_writer),
            window_size.clone(),
            colors,
        );

        let config = Config {
            scrolling_history: scrollback,
            kitty_keyboard: true,
            ..Config::default()
        };
        let dimensions = TermDimensions::new(cols, rows);
        let term = Term::new(config, &dimensions, event_proxy);
        let term = Arc::new(FairMutex::new(term));

        let (byte_tx, byte_rx) = crossbeam_channel::bounded(256);
        let dirty = Arc::new(AtomicBool::new(false));
        let (wake_tx, wake_rx) = async_channel::bounded(1);

        let pty_master: Arc<Mutex<Box<dyn MasterPty + Send>>> = Arc::new(Mutex::new(pair.master));

        let child_pid = child.process_id();
        let child = Mutex::new(Some(child));

        drop(pair.slave);

        let reader_dirty = Arc::clone(&dirty);
        let reader_handle = thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                Self::reader_loop(reader, byte_tx, reader_dirty, wake_tx);
            })
            .map_err(SessionError::ReaderThread)?;

        let processor = Processor::new();

        Ok(Self {
            term,
            processor,
            pty_master,
            pty_writer,
            child,
            child_pid,
            shell_type: detected_shell,
            byte_rx,
            dirty,
            wake_rx: Some(wake_rx),
            event_rx,
            zone_tracker: SemanticZoneTracker::new(),
            window_size,
            _reader_handle: reader_handle,
        })
    }

    fn reader_loop(
        mut reader: Box<dyn Read + Send>,
        byte_tx: Sender<Vec<u8>>,
        dirty: Arc<AtomicBool>,
        wake_tx: async_channel::Sender<()>,
    ) {
        let mut buf = [0u8; 16384];

        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    debug!("PTY reader: EOF");
                    break;
                }
                Ok(n) => {
                    if byte_tx.send(buf[..n].to_vec()).is_err() {
                        debug!("PTY reader: byte channel closed");
                        break;
                    }
                    if !dirty.swap(true, Ordering::AcqRel) {
                        let _ = wake_tx.try_send(());
                    }
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    warn!("PTY reader error: {}", e);
                    break;
                }
            }
        }
    }

    /// Drain pending bytes from the reader thread and feed them into the terminal parser.
    /// Returns true if any bytes were processed (terminal is dirty and needs repaint).
    ///
    /// Processes at most 64 chunks per call to keep FairMutex hold times bounded.
    /// The caller should re-check or schedule another render if the channel isn't empty.
    const MAX_CHUNKS_PER_DRAIN: usize = 64;

    pub fn process_pending_bytes(&mut self) -> bool {
        let mut dirty = false;
        let mut term = self.term.lock();

        for _ in 0..Self::MAX_CHUNKS_PER_DRAIN {
            match self.byte_rx.try_recv() {
                Ok(bytes) => {
                    self.processor.advance(&mut *term, &bytes);
                    dirty = true;
                }
                Err(_) => break,
            }
        }

        dirty
    }

    pub fn term_arc(&self) -> Arc<FairMutex<Term<EventProxy>>> {
        Arc::clone(&self.term)
    }

    pub fn pty_master_arc(&self) -> Arc<Mutex<Box<dyn MasterPty + Send>>> {
        Arc::clone(&self.pty_master)
    }

    pub fn take_wake_rx(&mut self) -> Option<async_channel::Receiver<()>> {
        self.wake_rx.take()
    }

    pub fn dirty(&self) -> &Arc<AtomicBool> {
        &self.dirty
    }

    pub fn has_pending_bytes(&self) -> bool {
        !self.byte_rx.is_empty()
    }

    pub fn write(&self, bytes: &[u8]) {
        let mut writer = self.pty_writer.lock();
        if let Err(e) = writer.write_all(bytes) {
            warn!("PTY write error: {}", e);
        }
        if let Err(e) = writer.flush() {
            warn!("PTY flush error: {}", e);
        }
    }

    pub fn resize(&self, cols: usize, rows: usize) {
        let cols = cols.min(u16::MAX as usize).max(1);
        let rows = rows.min(u16::MAX as usize).max(1);
        {
            let master = self.pty_master.lock();
            if let Err(e) = master.resize(PtySize {
                rows: rows as u16,
                cols: cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            }) {
                warn!("PTY resize error: {}", e);
            }
        }

        let mut term = self.term.lock();
        let dimensions = TermDimensions::new(cols, rows);
        term.resize(dimensions);
    }

    pub fn try_recv_event(&self) -> Option<SessionEvent> {
        self.event_rx.try_recv().ok()
    }

    pub fn mode(&self) -> TermMode {
        let term = self.term.lock();
        *term.mode()
    }

    pub fn window_size(&self) -> &SharedWindowSize {
        &self.window_size
    }

    pub fn zone_tracker(&self) -> &SemanticZoneTracker {
        &self.zone_tracker
    }

    pub fn zone_tracker_mut(&mut self) -> &mut SemanticZoneTracker {
        &mut self.zone_tracker
    }

    pub fn kill(&self) {
        if let Some(ref mut child) = *self.child.lock() {
            let _ = child.kill();
        }
    }

    /// Get the PID of the shell child process (cached at spawn time).
    pub fn process_id(&self) -> Option<u32> {
        self.child_pid
    }

    /// Get the detected shell type for this session.
    pub fn shell_type(&self) -> crate::shell_integration::ShellType {
        self.shell_type
    }

    /// Query the current working directory of the shell process.
    /// Uses platform-specific APIs (proc_pidinfo on macOS, /proc on Linux).
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        self.process_id().and_then(crate::cwd::process_cwd)
    }
}

impl Drop for SessionEngine {
    fn drop(&mut self) {
        if let Some(ref mut child) = *self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn feed_bytes_to_term<T: EventListener>(
    term: &mut Term<T>,
    processor: &mut Processor,
    bytes: &[u8],
) {
    processor.advance(term, bytes);
}
