use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("failed to create PTY: {0}")]
    PtyCreation(#[source] anyhow::Error),

    #[error("failed to spawn shell '{shell}': {source}")]
    ShellSpawn {
        shell: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("failed to obtain PTY reader: {0}")]
    PtyReader(#[source] anyhow::Error),

    #[error("failed to obtain PTY writer: {0}")]
    PtyWriter(#[source] anyhow::Error),

    #[error("failed to spawn reader thread: {0}")]
    ReaderThread(#[source] std::io::Error),

    #[error("failed to prepare shell integration: {0}")]
    ShellIntegration(#[source] std::io::Error),
}
