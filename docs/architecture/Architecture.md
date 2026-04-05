# OrcaShell: Architecture

## System Overview

OrcaShell is a single Rust binary that embeds both a GPUI desktop UI and an orchestration
daemon core. The UI is a client of the daemon core. The `orcash` CLI is a separate binary
that communicates with the daemon core over the platform-native local IPC transport:
Unix domain sockets on macOS/Linux and named pipes on Windows.

```
┌───────────────────────────────────────────────────────────────────────┐
│                           OrcaShell App                               │
│                        (single Rust binary)                           │
│                                                                       │
│  ┌──────────────────────────┐     ┌─────────────────────────────────┐ │
│  │           UI             │     │     Orchestration Core          │ │
│  │         (GPUI)           │     │       (daemon logic)            │ │
│  │                          │     │                                 │ │
│  │  ┌──────────────┐        │     │  ┌──────────────┐               │ │
│  │  │ Task Panel   │        │     │  │ Task Tracker │               │ │
│  │  ├──────────────┤        │     │  ├──────────────┤               │ │
│  │  │ Agent Status │        │◀────┼──│ Event Bus     │               │ │
│  │  ├──────────────┤        │     │  ├──────────────┤               │ │
│  │  │ Worktree UI  │        │     │  │ Agent Adapters│               │ │
│  │  ├──────────────┤        │     │  ├──────────────┤               │ │
│  │  │ Review UI    │        │     │  │ Git Manager   │               │ │
│  │  └──────────────┘        │     │  ├──────────────┤               │ │
│  │                          │     │  │ Session Broker│               │ │
│  │  ┌────────────────────┐  │     │  │  (PTY + Term) │               │ │
│  │  │ Pane Manager       │  │     │  ├──────────────┤               │ │
│  │  │ (splits/tabs)      │  │     │  │ SQLite Store  │               │ │
│  │  └───────┬────────────┘  │     │  └──────────────┘               │ │
│  │          │               │     └─────────────────────────────────┘ │
│  │  ┌───────▼────────────┐  │                                        │
│  │  │ Terminal Panes     │  │      Local IPC transport               │
│  │  │ (GPUI views)       │  │◀──────────────────────────────────┐   │
│  │  │ alacritty_terminal  │  │                                   │   │
│  │  │ + PTY I/O           │  │                                   │   │
│  │  └─────────────────────┘  │                                   │   │
│  └──────────────────────────┘                                   │   │
└───────────────────────────────────────────────────────────────────┬──┘
                                                                    │
                                                          ┌─────────▼──────────┐
                                                          │   orcash CLI        │
                                                          │ (human + agent API) │
                                                          └─────────────────────┘
```

## Process Model

The current product runs as one process with the daemon logic embedded. Future headless
mode (`orcashelld`) runs the daemon core standalone with tokio for remote clients.

- `orcashell` (current): desktop app + embedded daemon
- `orcashelld` (future): headless daemon for remote/web clients
- `orcashell --attach` (future): attach UI to an existing daemon

## Core Concepts

### Task
A unit of orchestration. Always has a worktree, optionally an agent session, and a durable
lifecycle state.

```rust
pub struct Task {
    pub id: TaskId,
    pub description: String,
    pub assigned_agent: Option<AgentId>,
    pub worktree_id: WorktreeId,
    pub branch: String,
    pub status: TaskStatus,          // pending|active|review|merging|done|failed
    pub parent_branch: String,
    pub created_at: DateTime<Utc>,
}
```

### Session
A PTY + terminal emulation state + process lifecycle.

```rust
pub struct Session {
    pub id: SessionId,
    pub task_id: TaskId,
    pub pty: PtyHandle,
    pub term: TermHandle,
    pub state: SessionState,         // running|paused|exited
    pub started_at: DateTime<Utc>,
    pub exited_at: Option<DateTime<Utc>>,
}
```

### TermHandle
Thread-safe shared terminal state between the session engine and the UI.

```rust
pub struct TermHandle {
    pub term: Arc<FairMutex<Term<()>>>,
    pub cols: u16,
    pub rows: u16,
}
```

### Worktree
One directory per task, created and managed by the daemon core.

```
repo/
└── .orcashell/
    └── worktrees/
        ├── task-001/
        ├── task-002/
        └── task-003/
```

`.orcashell/` is excluded from version control via `.git/info/exclude`.

### ReviewRun
Ties together a reviewed task, reviewer session, injected diff or context, and verdict.

### ContextBundle
Handoff primitive. Bounded payload of captured turns plus optional git artifacts, stored in
SQLite, injectable into another session.

## Concurrency Model

- **GPUI main thread.** Runs the UI event loop and GPUI's smol executor. Handles rendering,
  input events, and UI state.
- **PTY reader threads.** One dedicated OS thread per active session. Reads PTY bytes,
  feeds into `alacritty_terminal::Term`, signals UI for repaint.
- **Socket server thread.** Accepts `orcash` CLI connections, deserializes messages, routes
  to daemon core.
- **SQLite thread.** Dedicated thread for database operations. Commands sent via crossbeam
  channel, results returned via oneshot channel.
- **Git operation threads.** Short-lived threads spawned for git CLI operations (worktree
  create, merge, diff).
- **Event bus.** In-process crossbeam channels connecting all components.

```
GPUI main thread (smol executor) ←→ crossbeam channels ←→ daemon core threads
                                                          ├── PTY reader thread (per session)
                                                          ├── Socket server thread
                                                          ├── SQLite thread
                                                          └── Git operation threads (spawned)
```

Runtime-agnostic trait boundaries in the daemon core enable future tokio adoption for the
headless daemon without rewriting core logic.

## Agent Adapter Model

Adapters provide per-agent knowledge for structured workflows.

```rust
pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> AgentId;
    fn launch(&self, task: &Task, worktree: &Worktree) -> LaunchPlan;
    fn bootstrap_text(&self, task: &Task) -> String;
    fn inject_message(&self, session: &Session, msg: InjectedMessage) -> anyhow::Result<()>;
    fn turn_sentinel(&self, turn_id: TurnId) -> String;
}
```

The current product implements `ClaudeCodeAdapter` only. Other agents spawn in PTYs
without structured turn capture.

## Terminal Rendering

```
PTY Reader Thread
  └─ reads bytes from PTY fd
     └─ sends to SessionEngine (bounded crossbeam channel)

SessionEngine (per terminal session)
  ├─ feeds bytes into alacritty_terminal VTE parser + Term
  ├─ tracks dirty regions
  └─ posts "needs repaint" to GPUI (coalesced, max 120fps)

GPUI TerminalView (per pane)
  ├─ on repaint: locks TermHandle, reads cell grid, renders glyphs
  └─ on input: translates key/mouse events to PTY writes
```

## Storage

SQLite schema (high-level):
- `repos` - tracked repositories
- `tasks` - task metadata and lifecycle state
- `worktrees` - worktree paths and branch associations
- `sessions` - session metadata (not raw PTY bytes)
- `turns` - structured turn captures (bounded)
- `handoffs` - ContextBundle JSON blobs
- `review_runs` - review artifacts and verdicts
- `events` - append-only audit log

## IPC Protocol

CLI-to-daemon communication uses serde-serialized messages over the local platform IPC
transport. Message types are defined in the `orcashell-protocol` crate:

- **Commands:** request/response (create task, spawn session, resize, handoff)
- **Events:** pub/sub notifications (task updated, session state changed)

The protocol is versioned to support future headless daemon scenarios.

## Environment Variables (Agent Sessions)

When the daemon spawns an agent, it injects:

```bash
ORCA_SOCKET=/tmp/orcashell.sock
ORCA_TASK_ID=task-001
ORCA_AGENT_ID=claude-code
ORCA_BRANCH=orca/task-001/feature-name
ORCA_WORKTREE=/repo/.orcashell/worktrees/task-001
```

On Windows, `ORCA_SOCKET` contains the named-pipe endpoint (for example,
`\\.\pipe\orcashell-<user-sid>`).

Agents discover their context from the environment and call `orcash` CLI commands to
communicate with the orchestrator.
