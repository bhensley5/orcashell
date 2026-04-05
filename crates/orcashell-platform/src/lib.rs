//! Platform helpers for OrcaShell.
//!
//! Provides cross-platform abstractions for subprocess spawning, file replacement,
//! home directory resolution, and opening URLs in the user's default browser.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Create a [`Command`] for the given program with platform-appropriate defaults.
///
/// On Windows, sets `CREATE_NO_WINDOW` to prevent a console window from flashing
/// when spawning background processes (git, shell detection probes, etc.).
pub fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Replace `dest` with the contents of `temp`, removing `temp` in the process.
///
/// On Unix this is a simple atomic `rename(2)`. On Windows, uses `MoveFileExW`
/// with `MOVEFILE_REPLACE_EXISTING` and a bounded retry policy to handle
/// transient locks from antivirus or file indexing.
pub fn replace_file(temp: &Path, dest: &Path) -> io::Result<()> {
    imp::replace_file(temp, dest)
}

/// Return the current user's home directory.
///
/// Prefers [`dirs::home_dir`] (which uses platform-native APIs). Falls back to
/// `$HOME` on Unix or `%USERPROFILE%` on Windows.
pub fn user_home_dir() -> Option<PathBuf> {
    dirs::home_dir().or_else(|| {
        #[cfg(unix)]
        {
            std::env::var_os("HOME").map(PathBuf::from)
        }
        #[cfg(windows)]
        {
            std::env::var_os("USERPROFILE").map(PathBuf::from)
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    })
}

/// Open a web URL in the platform-default browser.
///
/// Returns `false` if the URL scheme is unsupported or the spawn fails.
pub fn open_url(url: &str) -> bool {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return false;
    }

    #[cfg(target_os = "windows")]
    {
        command("explorer").arg(url).spawn().is_ok()
    }

    #[cfg(target_os = "macos")]
    {
        command("open").arg(url).spawn().is_ok()
    }

    #[cfg(target_os = "linux")]
    {
        command("xdg-open").arg(url).spawn().is_ok()
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

// ── Platform-specific implementation ────────────────────────────────

#[cfg(unix)]
mod imp {
    use std::io;
    use std::path::Path;

    pub fn replace_file(temp: &Path, dest: &Path) -> io::Result<()> {
        std::fs::rename(temp, dest)
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_ACCESS_DENIED, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    const MAX_RETRIES: u32 = 5;
    const BASE_DELAY_MS: u64 = 50;

    fn to_wide(path: &Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn is_retryable(code: u32) -> bool {
        matches!(
            code,
            ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION
        )
    }

    pub fn replace_file(temp: &Path, dest: &Path) -> io::Result<()> {
        let wide_src = to_wide(temp);
        let wide_dst = to_wide(dest);
        let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;

        for attempt in 0..MAX_RETRIES {
            let ok = unsafe { MoveFileExW(wide_src.as_ptr(), wide_dst.as_ptr(), flags) };
            if ok != 0 {
                return Ok(());
            }

            let err = unsafe { GetLastError() };
            if !is_retryable(err) || attempt + 1 == MAX_RETRIES {
                // Best-effort cleanup of the temp file on final failure.
                let _ = std::fs::remove_file(temp);
                return Err(io::Error::from_raw_os_error(err as i32));
            }

            let delay = BASE_DELAY_MS * (1 << attempt);
            thread::sleep(Duration::from_millis(delay));
        }

        unreachable!()
    }
}

// Fallback for platforms that are neither Unix nor Windows (shouldn't happen
// for our supported targets, but keeps the code compilable everywhere).
#[cfg(not(any(unix, windows)))]
mod imp {
    use std::io;
    use std::path::Path;

    pub fn replace_file(temp: &Path, dest: &Path) -> io::Result<()> {
        std::fs::rename(temp, dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("data.tmp");
        let dest = dir.path().join("data.json");

        std::fs::write(&temp, b"hello").unwrap();
        replace_file(&temp, &dest).unwrap();

        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hello");
        assert!(!temp.exists(), "temp file should be removed after replace");
    }

    #[test]
    fn replace_file_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("data.tmp");
        let dest = dir.path().join("data.json");

        std::fs::write(&dest, b"old content").unwrap();
        std::fs::write(&temp, b"new content").unwrap();
        replace_file(&temp, &dest).unwrap();

        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new content");
        assert!(!temp.exists(), "temp file should be removed after replace");
    }
}
