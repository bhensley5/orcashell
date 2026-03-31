# Changelog

All notable changes to OrcaShell will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **GPU-accelerated terminal emulator** built on GPUI with damage-aware line caching, background cell merging, and text run batching for efficient rendering.
- **Kitty keyboard protocol** support (levels 1, 4, 5) with full CSI-u encoding, press/repeat/release events, modifier-only keys, and associated text reporting.
- **SGR 1006 mouse reporting** with click, drag, and motion tracking. Character, word, and line selection via single/double/triple click.
- **Selection-delete** for shell input. Highlight text in your prompt and press Delete/Backspace to remove it in place. Uses OSC 133 semantic zones to constrain operations to the input region.
- **OSC 133 shell integration** with automatic hook injection for zsh and bash. Tracks prompt, input, executing, and command-complete states. Detects semantic zones for prompt/input/output regions.
- **Programmatic box-drawing** (U+2500-U+257F) and **block element rendering** (U+2580-U+259F) via GPU paths for pixel-perfect alignment across any font.
- **Five underline variants**: single, double, wavy, dotted, dashed. Plus hyperlink rendering with colored underlines and strikethrough support.
- **True color** (24-bit RGB), 256-color mode, and 16 ANSI colors with a custom pastel neon palette.
- **Regex search** across entire scrollback history with smart case sensitivity, match count display, current/other match highlighting, and wraparound navigation.
- **Cursor shapes**: block, bar, and underline with configurable blink animation.
- **Scrollbar** with proportional thumb, click-to-scroll, and drag tracking.
- **Bracketed paste** support.
- **Working directory tracking** via proc_pidinfo (macOS) and /proc (Linux).
- **Configurable scrollback** from 100 to 100,000 lines.
- **Zoom in/out** with pixel-level font size adjustment.
- **Multi-window support** with independent workspace state per window.
- **Horizontal and vertical splits** with draggable dividers and nested layouts.
- **Tabbed terminals** with Cmd+T to create, Cmd+1-9 to switch, and inline rename.
- **Sidebar** with project tree, terminal list, git status badges, worktree indicators, activity pulses, notification badges, drag-and-drop reordering, and context menus.
- **Window tab bar** with terminal tabs, auxiliary tabs (settings, diff), keyboard shortcut badges, and inline rename.
- **Activity pulse animation** on terminals with recent output. Pulsing indicator in sidebar so you can see which terminals are active at a glance.
- **Agent notification system** with configurable urgent patterns. Detects when agents post approval prompts or permission requests and surfaces notification badges in the sidebar. Cleared on focus.
- **Diff explorer** with two-pane layout: file tree (staged/unstaged sections) on the left, syntax-highlighted diff content on the right. Word-level inline change detection via imara-diff. Dual line-number gutters. Virtualized scrolling. Multi-file selection for batch staging.
- **Git operations**: stage, unstage, commit, push, pull, and merge-back. All routed through a multi-worker daemon with scope-level concurrency control and async event broadcasting.
- **Managed git worktrees**: create isolated worktrees from the context menu with auto-generated branches. Merge back with conflict detection. Full lifecycle management including cleanup and optional branch deletion.
- **Git file status detection**: added, modified, deleted, renamed, typechange, untracked, and conflicted files with per-file insertion/deletion counts.
- **Upstream tracking** with ahead/behind commit counts.
- **Syntax highlighting** via syntect, integrated into diff lines with stateful per-file highlighting.
- **Settings view** with font size, font family, scrollback lines, cursor style/blink, activity pulse toggle, agent notification toggle, customizable urgent patterns, and default shell override. All auto-saved with debounce.
- **SQLite persistence** for window positions, project layouts, split configurations, terminal working directories, custom names, and worktree associations. WAL mode for concurrent access. Everything survives restarts.
- **Context menus** throughout the UI (projects, terminals, tabs, diff files).
- **Status bar** showing project name, path, shell, and pane position.
- **Orca Brutalism design system**: neo-brutalist structure with deep ocean dark mode, bioluminescent pastel neon accents, and the orca spectrum rule (no pure black, no pure white).
- **Daemon architecture** with Unix socket IPC, length-prefixed JSON framing, protocol version negotiation, and multi-threaded git coordinator with dedicated worker pools for snapshots, diffs, local mutations, and remote operations.
- **CLI client** (`orca daemon status`) for querying daemon health over Unix socket.
- **OSC 10/11/12 color query responses** for terminal color negotiation.
- **CJK double-width character** support.
