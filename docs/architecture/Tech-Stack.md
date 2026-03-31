# OrcaShell: Tech Stack

## Core Dependencies

| Component | Crate / Tool | Version Strategy | Rationale |
|-----------|-------------|-----------------|-----------|
| **UI Framework** | `gpui` | crates.io (`=0.2.2`) | GPU-accelerated, native Rust, no webview. Published by the Zed team. |
| **Terminal Emulation** | `alacritty_terminal` | crates.io (`0.25.1` stable) | Battle-tested VTE parser and terminal state. Apache-2.0. Used by Alacritty, Zed, Okena. |
| **PTY Abstraction** | `portable-pty` | crates.io (`0.9.0`) | Cross-platform PTY (Unix + Windows ConPTY). From WezTerm. |
| **Persistence** | `rusqlite` | crates.io (latest) | Lightweight SQLite bindings. bundled feature for zero system deps. |
| **Serialization** | `serde` + `serde_json` | crates.io (latest) | Protocol messages, config, storage. |
| **CLI Parsing** | `clap` | crates.io (v4) | Derive-based CLI for `orca` binary. |
| **Channels** | `crossbeam-channel` | crates.io (latest) | Fast MPMC channels for daemon core event bus. |
| **Error Handling** | `anyhow` + `thiserror` | crates.io (latest) | `anyhow` for application errors, `thiserror` for library error types. |
| **Logging** | `tracing` + `tracing-subscriber` | crates.io (latest) | Structured logging with spans. |
| **Time** | `chrono` | crates.io (latest) | DateTime handling for task/session timestamps. |
| **UUID** | `uuid` | crates.io (latest, v4 feature) | Unique IDs for tasks, sessions, worktrees. |
| **Git Operations** | `git2` | crates.io (`default-features = false, features = ["vendored-libgit2"]`) | Branch detection, diff stats/hunks, worktree management, merge + conflict detection. Vendored libgit2 for zero system deps. Cross-platform (macOS/Linux/Windows). |

## Explicitly NOT Used (MVP)

| Technology | Reason |
|-----------|--------|
| **gpui-terminal crate as a runtime dependency** | We do not depend on the published crate. Phase 1 vendors the local reference source into `crates/orcashell-terminal-view/` as a starting point so we own the implementation. |
| **Tauri / webview / React** | Rejected in architecture review. GPUI provides native rendering without JS overhead. |
| **gix / gitoxide** | Evaluated but git2 chosen instead. gix worktree support has gaps and API is still Tier 2 (breaking changes). Revisit if gix reaches Tier 1. |
| **warp / axum / hyper** | No HTTP server in MVP. Socket server is raw Unix domain socket. |

## Workspace Layout

```
orcashell/
├── Cargo.toml                    # Workspace root
├── CLAUDE.md                     # Execution contract
├── crates/
│   ├── orcashell/                # (bin) GPUI desktop app + embedded daemon
│   │   └── src/main.rs
│   │
│   ├── orcashell-ui/                  # (lib) GPUI views: layout, panels, actions
│   │   └── src/
│   │       ├── app_view.rs       # Root view (task sidebar + pane area)
│   │       ├── panes.rs          # Pane tree state + split/tab commands
│   │       ├── task_panel.rs     # Task list sidebar
│   │       ├── review_panel.rs   # Review status display
│   │       └── theme.rs          # Color scheme (orca-inspired black/white/blue)
│   │
│   ├── orcashell-terminal-view/       # (lib) GPUI terminal widget
│   │   └── src/
│   │       ├── terminal_view.rs  # GPUI View that renders cell grid
│   │       ├── input_map.rs      # Key/mouse → PTY bytes (bracketed paste, etc.)
│   │       └── render.rs         # Glyph/layout caching, damage-based redraw
│   │
│   ├── orcashell-session/             # (lib) PTY + Term engine (SessionEngine)
│   │   └── src/
│   │       ├── engine.rs         # PTY read/write loops + Term updates
│   │       ├── pty.rs            # portable-pty wrapper + sizing + process kill
│   │       └── turn_capture.rs   # Sentinel bracketing + ANSI strip
│   │
│   ├── orcashell-daemon-core/         # (lib) Orchestration core
│   │   └── src/
│   │       ├── daemon.rs         # Top-level daemon coordinator
│   │       ├── task.rs           # Task lifecycle state machine
│   │       ├── review.rs         # Review workflow orchestration
│   │       ├── handoff.rs        # Context handoff logic
│   │       ├── bus.rs            # Event bus (crossbeam channels)
│   │       ├── agents/
│   │       │   ├── mod.rs        # AgentAdapter trait
│   │       │   └── claude_code.rs # Claude Code adapter
│   │       └── socket_server.rs  # Unix domain socket server for orca CLI
│   │
│   ├── orcashell-store/               # (lib) SQLite schema, migrations, queries
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── schema.rs         # Table definitions + migrations
│   │       └── queries.rs        # Typed query functions
│   │
│   ├── orcashell-git/                 # (lib) Git operations (worktree, diff, merge)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── worktree.rs       # Create/destroy/list worktrees
│   │       ├── merge.rs          # Merge attempt + conflict detection
│   │       └── diff.rs           # Diff generation for reviews
│   │
│   ├── orcashell-protocol/            # (lib) Shared types + versioned messages
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── messages.rs       # All CLI ↔ daemon message types
│   │       └── types.rs          # TaskId, SessionId, AgentId, etc.
│   │
│   └── orcashell-cli/                 # (bin) `orca` command-line interface
│       └── src/
│           ├── main.rs
│           ├── client.rs         # Socket client to daemon
│           └── commands/
│               ├── task.rs
│               ├── review.rs
│               ├── report.rs
│               ├── handoff.rs
│               ├── worktree.rs
│               └── daemon.rs
│
├── assets/                       # Icons, default themes
├── docs/                         # Project documentation
└── project-start-new/            # Project-start skill (reference only)
```

## Build and Test Commands

```bash
# Build entire workspace
cargo build --workspace

# Run all tests
cargo test --workspace

# Lint
cargo clippy --workspace -- -D warnings

# Format check
cargo fmt --check

# Run the desktop app
cargo run --bin orcashell

# Run the CLI
cargo run --bin orca -- <command>
```

## Platform-Specific Notes

### macOS (Primary)
- GPUI renders via Metal (or wgpu → Metal)
- PTY via standard Unix PTY (portable-pty)
- IPC via Unix domain socket at `/tmp/orcashell.sock`

### Linux (Secondary)
- GPUI renders via Vulkan (or wgpu → Vulkan)
- PTY via standard Unix PTY (portable-pty)
- IPC via Unix domain socket at `/tmp/orcashell.sock`
- May need system packages: `libxkbcommon-dev`, `libwayland-dev`, Vulkan drivers

### Windows (Future)
- GPUI renders via DirectX 12 (wgpu → DX12)
- PTY via ConPTY (portable-pty)
- IPC via named pipe (`\\.\pipe\orcashell`)
- Shells: PowerShell, Git Bash, WSL
- Agent compatibility varies (Claude Code needs Git Bash/WSL, Gemini CLI native)
