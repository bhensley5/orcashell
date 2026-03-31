use anyhow::{bail, Context, Result};
use orcashell_protocol::messages::{ClientCommand, DaemonResponse, OpenDisposition};
use std::path::PathBuf;

pub fn open_project(dir: PathBuf, new_window: bool) -> Result<()> {
    // Canonicalize to resolve symlinks and relative components before sending.
    // Fall back to absolute-ifying relative paths if canonicalize fails (e.g. dir not yet created).
    let path = std::fs::canonicalize(&dir).unwrap_or_else(|_| {
        if dir.is_absolute() {
            dir.clone()
        } else {
            std::env::current_dir().unwrap_or_default().join(&dir)
        }
    });

    let path_str = path
        .to_str()
        .context("path contains non-UTF-8 characters")?
        .to_string();

    // Validate before attempting any launch so both warm and cold paths give a consistent error.
    if !path.is_dir() {
        anyhow::bail!("not a directory: {path_str}");
    }

    let disposition = if new_window {
        OpenDisposition::NewWindow
    } else {
        OpenDisposition::NewTab
    };

    match crate::client::send_command(ClientCommand::OpenProject {
        path: path_str.clone(),
        disposition,
    }) {
        Ok(DaemonResponse::ProjectOpened { .. }) => {
            println!("Opened: {path_str}");
            Ok(())
        }
        Ok(DaemonResponse::Error { message }) => {
            bail!("daemon error: {message}")
        }
        Ok(other) => bail!("unexpected daemon response: {other:?}"),
        Err(_) => {
            // Daemon not running. Launch the app with the directory as an argument.
            launch_app_with_path(&path_str, new_window)
        }
    }
}

/// Launch the OrcaShell app with `--open-dir` / `--open-new-window` args.
/// The app picks these up after startup and routes them through the normal open-project queue.
fn launch_app_with_path(path: &str, new_window: bool) -> Result<()> {
    let mut args: Vec<&str> = vec!["--open-dir", path];
    if new_window {
        args.push("--open-new-window");
    }

    #[cfg(target_os = "macos")]
    {
        // macOS: use `open -n -a OrcaShell --args ...`
        //   -n: always launch a new instance (never re-activate an existing one), so
        //       --open-dir args are guaranteed to be received by a fresh main().
        //   If a daemon is already running the new instance will connect to it and
        //   immediately pick up the enqueued open-project request normally.
        std::process::Command::new("open")
            .arg("-n")
            .arg("-a")
            .arg("OrcaShell")
            .arg("--args")
            .args(&args)
            .spawn()
            .context("failed to invoke `open -n -a OrcaShell`")?;
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Linux / Windows: orcashell[.exe] lives next to orcash[.exe].
        let exe = std::env::current_exe().context("cannot determine current executable path")?;
        let sibling = exe
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join(if cfg!(windows) {
                "orcashell.exe"
            } else {
                "orcashell"
            });
        anyhow::ensure!(
            sibling.exists(),
            "orcashell binary not found at {}",
            sibling.display()
        );
        std::process::Command::new(&sibling)
            .args(&args)
            .spawn()
            .with_context(|| format!("failed to spawn {}", sibling.display()))?;
    }

    Ok(())
}
