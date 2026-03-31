# Contributing to OrcaShell

Thanks for your interest in contributing. This doc covers everything you need to build, test, and submit changes.

## Prerequisites

You'll need:

- **Rust** (stable, 2021 edition): https://rustup.rs
- **macOS** or **Linux** (Windows support is planned but not yet available)

That's it. All dependencies (including SQLite) are bundled via Cargo.

## Building from source

```bash
# Debug build
cargo build --workspace

# Release build
cargo build --workspace --release

# Run the app
cargo run -p orcashell

# Run the CLI
cargo run -p orcashell-cli -- daemon status
```

## Running tests

```bash
# All tests
cargo test --workspace

# Clippy (warnings are errors)
cargo clippy --workspace -- -D warnings

# Format check
cargo fmt --check
```

All three must pass before submitting a PR. If clippy or fmt fails, fix it before pushing.

## Project structure

```
crates/
  orcashell/                 # Desktop app entry point (GPUI)
  orcashell-ui/              # Workspace, sidebar, tab bar, diff explorer, settings
  orcashell-terminal-view/   # GPU terminal renderer, input, mouse, search, colors
  orcashell-session/         # PTY engine, shell integration, semantic zones
  orcashell-git/             # libgit2 wrapper: status, diff, stage, commit, worktrees
  orcashell-syntax/          # Syntax highlighting via syntect
  orcashell-daemon-core/     # Git coordinator, Unix socket server, worker threads
  orcashell-store/           # SQLite persistence (windows, projects, worktrees)
  orcashell-protocol/        # IPC framing and message types
  orcashell-cli/             # CLI client (orca command)

forks/
  alacritty_terminal/        # Forked with semantic prompt event support
  vte/                       # Forked with OSC 133 parsing
```

### Key architecture rules

- The UI is a client of the daemon core. It never mutates git state or spawns processes directly.
- All git operations flow through the git coordinator's worker pool.
- Terminal rendering is decoupled from terminal state via frame snapshots (lock-free rendering).
- All persistent state goes through SQLite (in `orcashell-store`). No ad-hoc file writes.

## Coding conventions

- **Rust style:** Standard `rustfmt` formatting. Run `cargo fmt` before committing.
- **Error handling:** Use `anyhow::Result` for application code, `thiserror` for library error types. Don't panic in library code.
- **Tests:** Every change that touches behavior should have a test. Unit tests go in `#[cfg(test)]` modules. Integration tests go in per-crate `tests/` directories.
- **Dependencies:** Minimize external crates. Every new dependency needs justification. Check if existing deps already cover it.
- **No `unsafe`** without explicit justification.
- **No hardcoded colors** in UI code. All colors come from theme tokens in `orcashell-ui/src/theme.rs`.
- **No pure black (`#000000`) or pure white (`#FFFFFF`)** anywhere in the UI. This is the Orca Brutalism rule.

## Submitting a PR

1. Fork the repo and create a branch from `main`.
2. Make your changes. Write tests.
3. Run `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, and `cargo fmt --check`.
4. Open a PR against `main` with a clear description of what you changed and why.
5. Keep PRs focused. One feature or fix per PR.

## Reporting bugs

Open an issue with:

- What you expected to happen
- What actually happened
- Steps to reproduce
- Your OS and Rust version

## Feature requests

Open an issue describing the use case. "I'm building X and I need Y because Z" is much more useful than "it would be cool if...".

## License

By contributing, you agree that your contributions will be licensed under both the MIT License and the Apache License 2.0, at the user's option.
