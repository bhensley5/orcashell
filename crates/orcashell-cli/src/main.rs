mod client;
mod commands;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "orca", about = "OrcaShell CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Daemon management commands
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Open a directory in OrcaShell (starts app if not running)
    Open {
        /// Directory to open
        #[arg(short, long, value_name = "PATH")]
        dir: PathBuf,
        /// Open in a new window instead of a new tab
        #[arg(long)]
        new_window: bool,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Check daemon status
    Status,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Daemon { action } => match action {
            DaemonAction::Status => commands::daemon::daemon_status(),
        },
        Commands::Open { dir, new_window } => commands::open::open_project(dir, new_window),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
