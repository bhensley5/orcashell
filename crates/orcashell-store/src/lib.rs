mod settings;

use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{params, Connection};

pub use settings::{
    config_dir, database_path, settings_path, AppSettings, CursorStyle, ThemeId, ThemeMode,
};

/// A window as stored in the SQLite database.
pub struct StoredWindow {
    pub window_id: i64,
    pub bounds_x: Option<f32>,
    pub bounds_y: Option<f32>,
    pub bounds_width: f32,
    pub bounds_height: f32,
    pub active_project_id: Option<String>,
    pub sort_order: i32,
    pub is_open: bool,
}

/// A project as stored in the SQLite database.
/// Pure data - no GPUI entities.
pub struct StoredProject {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub layout_json: String,
    pub terminal_names_json: String,
    pub sort_order: i32,
    pub window_id: i64,
}

/// An Orca-managed linked worktree as stored in the SQLite database.
pub struct StoredWorktree {
    pub id: String,
    pub project_id: String,
    pub repo_root: PathBuf,
    pub path: PathBuf,
    pub worktree_name: String,
    pub branch_name: String,
    pub source_ref: String,
    pub primary_terminal_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResumableAgentKind {
    Codex,
    ClaudeCode,
}

impl ResumableAgentKind {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
        }
    }

    fn from_sql(value: &str) -> std::io::Result<Self> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude-code" => Ok(Self::ClaudeCode),
            other => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown resumable agent kind: {other}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAgentTerminal {
    pub terminal_id: String,
    pub project_id: String,
    pub agent_kind: ResumableAgentKind,
    pub cwd: PathBuf,
    pub updated_at: String,
}

/// SQLite-backed persistence store for projects and app state.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open a file-backed database, creating the schema if needed.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// Open an in-memory database for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS windows (
                window_id INTEGER PRIMARY KEY,
                bounds_x REAL,
                bounds_y REAL,
                bounds_width REAL NOT NULL DEFAULT 1200.0,
                bounds_height REAL NOT NULL DEFAULT 800.0,
                active_project_id TEXT,
                sort_order INTEGER NOT NULL DEFAULT 0,
                is_open INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                path TEXT NOT NULL,
                layout_json TEXT NOT NULL,
                terminal_names_json TEXT NOT NULL DEFAULT '{}',
                sort_order INTEGER NOT NULL DEFAULT 0,
                window_id INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS app_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS worktrees (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                repo_root TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                worktree_name TEXT NOT NULL,
                branch_name TEXT NOT NULL,
                source_ref TEXT NOT NULL,
                primary_terminal_id TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS agent_terminals (
                terminal_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                agent_kind TEXT NOT NULL,
                cwd TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now'))
            );",
        )?;
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_worktrees_project_id
                ON worktrees(project_id);

            CREATE INDEX IF NOT EXISTS idx_worktrees_primary_terminal_id
                ON worktrees(primary_terminal_id);

            CREATE INDEX IF NOT EXISTS idx_agent_terminals_project_id
                ON agent_terminals(project_id);

            CREATE INDEX IF NOT EXISTS idx_agent_terminals_agent_cwd
                ON agent_terminals(agent_kind, cwd);",
        )?;
        Ok(())
    }

    // ── Windows ──

    /// Save or update a window (upsert).
    pub fn save_window(&self, window: &StoredWindow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO windows (window_id, bounds_x, bounds_y, bounds_width, bounds_height, active_project_id, sort_order, is_open, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'), datetime('now'))
             ON CONFLICT(window_id) DO UPDATE SET
                 bounds_x = excluded.bounds_x,
                 bounds_y = excluded.bounds_y,
                 bounds_width = excluded.bounds_width,
                 bounds_height = excluded.bounds_height,
                 active_project_id = excluded.active_project_id,
                 sort_order = excluded.sort_order,
                 is_open = excluded.is_open,
                 updated_at = datetime('now')",
            params![
                window.window_id,
                window.bounds_x,
                window.bounds_y,
                window.bounds_width,
                window.bounds_height,
                window.active_project_id,
                window.sort_order,
                window.is_open,
            ],
        )?;
        Ok(())
    }

    /// Load open windows ordered by sort_order (for startup restore).
    pub fn load_windows(&self) -> Result<Vec<StoredWindow>> {
        self.load_windows_where("is_open = 1")
    }

    /// Load the first hibernated (closed) window, if any, for reactivation.
    pub fn load_hibernated_window(&self) -> Result<Option<StoredWindow>> {
        let mut windows = self.load_windows_where("is_open = 0")?;
        Ok(if windows.is_empty() {
            None
        } else {
            Some(windows.remove(0))
        })
    }

    fn load_windows_where(&self, condition: &str) -> Result<Vec<StoredWindow>> {
        let sql = format!(
            "SELECT window_id, bounds_x, bounds_y, bounds_width, bounds_height, active_project_id, sort_order, is_open
             FROM windows WHERE {condition} ORDER BY sort_order"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(StoredWindow {
                window_id: row.get(0)?,
                bounds_x: row.get(1)?,
                bounds_y: row.get(2)?,
                bounds_width: row.get(3)?,
                bounds_height: row.get(4)?,
                active_project_id: row.get(5)?,
                sort_order: row.get(6)?,
                is_open: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Mark a window as hibernated (closed but state preserved).
    pub fn hibernate_window(&self, window_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE windows SET is_open = 0, updated_at = datetime('now') WHERE window_id = ?1",
            params![window_id],
        )?;
        Ok(())
    }

    /// Delete a window and all its projects permanently.
    pub fn delete_window(&mut self, window_id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare("SELECT id FROM projects WHERE window_id = ?1")?;
            let project_ids: Vec<String> = stmt
                .query_map(params![window_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for project_id in project_ids {
                tx.execute(
                    "DELETE FROM worktrees WHERE project_id = ?1",
                    params![project_id],
                )?;
                tx.execute(
                    "DELETE FROM agent_terminals WHERE project_id = ?1",
                    params![project_id],
                )?;
            }
        }
        tx.execute(
            "DELETE FROM projects WHERE window_id = ?1",
            params![window_id],
        )?;
        tx.execute(
            "DELETE FROM windows WHERE window_id = ?1",
            params![window_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Get the next available window ID.
    pub fn next_window_id(&self) -> Result<i64> {
        let max_id: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(window_id), 0) FROM windows", [], |r| {
                    r.get(0)
                })?;
        Ok(max_id + 1)
    }

    /// Save a window and all its projects in a single transaction.
    pub fn save_window_state(
        &mut self,
        window: &StoredWindow,
        projects: &[StoredProject],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;

        // Upsert window
        tx.execute(
            "INSERT INTO windows (window_id, bounds_x, bounds_y, bounds_width, bounds_height, active_project_id, sort_order, is_open, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'), datetime('now'))
             ON CONFLICT(window_id) DO UPDATE SET
                 bounds_x = excluded.bounds_x,
                 bounds_y = excluded.bounds_y,
                 bounds_width = excluded.bounds_width,
                 bounds_height = excluded.bounds_height,
                 active_project_id = excluded.active_project_id,
                 sort_order = excluded.sort_order,
                 is_open = excluded.is_open,
                 updated_at = datetime('now')",
            params![
                window.window_id,
                window.bounds_x,
                window.bounds_y,
                window.bounds_width,
                window.bounds_height,
                window.active_project_id,
                window.sort_order,
                window.is_open,
            ],
        )?;

        // Delete removed projects for this window
        let current_ids: std::collections::HashSet<String> =
            projects.iter().map(|p| p.id.clone()).collect();
        {
            let mut stmt = tx.prepare("SELECT id FROM projects WHERE window_id = ?1")?;
            let existing: Vec<String> = stmt
                .query_map(params![window.window_id], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            for old_id in &existing {
                if !current_ids.contains(old_id) {
                    tx.execute(
                        "DELETE FROM worktrees WHERE project_id = ?1",
                        params![old_id],
                    )?;
                    tx.execute(
                        "DELETE FROM agent_terminals WHERE project_id = ?1",
                        params![old_id],
                    )?;
                    tx.execute("DELETE FROM projects WHERE id = ?1", params![old_id])?;
                }
            }
        }

        // Upsert projects
        for project in projects {
            tx.execute(
                "INSERT INTO projects (id, name, path, layout_json, terminal_names_json, sort_order, window_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'), datetime('now'))
                 ON CONFLICT(id) DO UPDATE SET
                     name = excluded.name,
                     path = excluded.path,
                     layout_json = excluded.layout_json,
                     terminal_names_json = excluded.terminal_names_json,
                     sort_order = excluded.sort_order,
                     window_id = excluded.window_id,
                     updated_at = datetime('now')",
                params![
                    project.id,
                    project.name,
                    project.path.to_string_lossy().as_ref(),
                    project.layout_json,
                    project.terminal_names_json,
                    project.sort_order,
                    project.window_id,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    // ── Projects ──

    /// Save or update a project (upsert). Preserves `created_at` on update.
    pub fn save_project(&self, project: &StoredProject) -> Result<()> {
        self.conn.execute(
            "INSERT INTO projects (id, name, path, layout_json, terminal_names_json, sort_order, window_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'), datetime('now'))
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 path = excluded.path,
                 layout_json = excluded.layout_json,
                 terminal_names_json = excluded.terminal_names_json,
                 sort_order = excluded.sort_order,
                 window_id = excluded.window_id,
                 updated_at = datetime('now')",
            params![
                project.id,
                project.name,
                project.path.to_string_lossy().as_ref(),
                project.layout_json,
                project.terminal_names_json,
                project.sort_order,
                project.window_id,
            ],
        )?;
        Ok(())
    }

    /// Load all projects ordered by sort_order (all windows).
    pub fn load_projects(&self) -> Result<Vec<StoredProject>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, path, layout_json, terminal_names_json, sort_order, window_id
             FROM projects ORDER BY sort_order",
        )?;
        let rows = stmt.query_map([], |row| {
            let path_str: String = row.get(2)?;
            Ok(StoredProject {
                id: row.get(0)?,
                name: row.get(1)?,
                path: PathBuf::from(path_str),
                layout_json: row.get(3)?,
                terminal_names_json: row.get(4)?,
                sort_order: row.get(5)?,
                window_id: row.get(6)?,
            })
        })?;

        let mut projects = Vec::new();
        for row in rows {
            projects.push(row?);
        }
        Ok(projects)
    }

    /// Load projects for a specific window, ordered by sort_order.
    pub fn load_projects_for_window(&self, window_id: i64) -> Result<Vec<StoredProject>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, path, layout_json, terminal_names_json, sort_order, window_id
             FROM projects WHERE window_id = ?1 ORDER BY sort_order",
        )?;
        let rows = stmt.query_map(params![window_id], |row| {
            let path_str: String = row.get(2)?;
            Ok(StoredProject {
                id: row.get(0)?,
                name: row.get(1)?,
                path: PathBuf::from(path_str),
                layout_json: row.get(3)?,
                terminal_names_json: row.get(4)?,
                sort_order: row.get(5)?,
                window_id: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Delete a project by ID.
    pub fn delete_project(&self, id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM worktrees WHERE project_id = ?1", params![id])?;
        tx.execute(
            "DELETE FROM agent_terminals WHERE project_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_agent_terminal(&self, row: &StoredAgentTerminal) -> Result<()> {
        let cwd = normalize_stored_path(&row.cwd)?;
        self.conn.execute(
            "INSERT INTO agent_terminals (
                terminal_id, project_id, agent_kind, cwd, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%d %H:%M:%f', 'now'))
             ON CONFLICT(terminal_id) DO UPDATE SET
                project_id = excluded.project_id,
                agent_kind = excluded.agent_kind,
                cwd = excluded.cwd,
                updated_at = strftime('%Y-%m-%d %H:%M:%f', 'now')",
            params![
                row.terminal_id,
                row.project_id,
                row.agent_kind.as_sql(),
                cwd.to_string_lossy().as_ref(),
            ],
        )?;
        Ok(())
    }

    pub fn delete_agent_terminal(&self, terminal_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM agent_terminals WHERE terminal_id = ?1",
            params![terminal_id],
        )?;
        Ok(())
    }

    pub fn load_agent_terminals_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<StoredAgentTerminal>> {
        let mut stmt = self.conn.prepare(
            "SELECT terminal_id, project_id, agent_kind, cwd, updated_at
             FROM agent_terminals
             WHERE project_id = ?1
             ORDER BY updated_at DESC, terminal_id DESC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            let agent_kind: String = row.get(2)?;
            Ok(StoredAgentTerminal {
                terminal_id: row.get(0)?,
                project_id: row.get(1)?,
                agent_kind: ResumableAgentKind::from_sql(&agent_kind).map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(err),
                    )
                })?,
                cwd: PathBuf::from(row.get::<_, String>(3)?),
                updated_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_agent_terminals_for_project(&self, project_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM agent_terminals WHERE project_id = ?1",
            params![project_id],
        )?;
        Ok(())
    }

    pub fn save_worktree(&self, worktree: &StoredWorktree) -> Result<()> {
        let repo_root = normalize_stored_path(&worktree.repo_root)?;
        let path = normalize_stored_path(&worktree.path)?;
        self.conn.execute(
            "INSERT INTO worktrees (
                id, project_id, repo_root, path, worktree_name, branch_name, source_ref, primary_terminal_id, created_at, updated_at
            )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'), datetime('now'))
             ON CONFLICT(id) DO UPDATE SET
                 project_id = excluded.project_id,
                 repo_root = excluded.repo_root,
                 path = excluded.path,
                 worktree_name = excluded.worktree_name,
                 branch_name = excluded.branch_name,
                 source_ref = excluded.source_ref,
                 primary_terminal_id = excluded.primary_terminal_id,
                 updated_at = datetime('now')",
            params![
                worktree.id,
                worktree.project_id,
                repo_root.to_string_lossy().as_ref(),
                path.to_string_lossy().as_ref(),
                worktree.worktree_name,
                worktree.branch_name,
                worktree.source_ref,
                worktree.primary_terminal_id,
            ],
        )?;
        Ok(())
    }

    pub fn load_worktrees_for_project(&self, project_id: &str) -> Result<Vec<StoredWorktree>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, repo_root, path, worktree_name, branch_name, source_ref, primary_terminal_id
             FROM worktrees
             WHERE project_id = ?1
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok(StoredWorktree {
                id: row.get(0)?,
                project_id: row.get(1)?,
                repo_root: PathBuf::from(row.get::<_, String>(2)?),
                path: PathBuf::from(row.get::<_, String>(3)?),
                worktree_name: row.get(4)?,
                branch_name: row.get(5)?,
                source_ref: row.get(6)?,
                primary_terminal_id: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn load_worktrees_for_repo_root(&self, repo_root: &Path) -> Result<Vec<StoredWorktree>> {
        let repo_root = normalize_stored_path(repo_root)?;
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, repo_root, path, worktree_name, branch_name, source_ref, primary_terminal_id
             FROM worktrees
             WHERE repo_root = ?1
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![repo_root.to_string_lossy().as_ref()], |row| {
            Ok(StoredWorktree {
                id: row.get(0)?,
                project_id: row.get(1)?,
                repo_root: PathBuf::from(row.get::<_, String>(2)?),
                path: PathBuf::from(row.get::<_, String>(3)?),
                worktree_name: row.get(4)?,
                branch_name: row.get(5)?,
                source_ref: row.get(6)?,
                primary_terminal_id: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_worktree(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM worktrees WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn find_worktree_by_path(&self, path: &Path) -> Result<Option<StoredWorktree>> {
        let path = normalize_stored_path(path)?;
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, repo_root, path, worktree_name, branch_name, source_ref, primary_terminal_id
             FROM worktrees
             WHERE path = ?1",
        )?;
        let mut rows = stmt.query(params![path.to_string_lossy().as_ref()])?;
        match rows.next()? {
            Some(row) => Ok(Some(StoredWorktree {
                id: row.get(0)?,
                project_id: row.get(1)?,
                repo_root: PathBuf::from(row.get::<_, String>(2)?),
                path: PathBuf::from(row.get::<_, String>(3)?),
                worktree_name: row.get(4)?,
                branch_name: row.get(5)?,
                source_ref: row.get(6)?,
                primary_terminal_id: row.get(7)?,
            })),
            None => Ok(None),
        }
    }

    /// Update sort_order for a list of project IDs (in display order).
    pub fn update_project_order(&mut self, ids_in_order: &[&str]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for (i, id) in ids_in_order.iter().enumerate() {
            tx.execute(
                "UPDATE projects SET sort_order = ?1 WHERE id = ?2",
                params![i as i32, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // ── App State ──

    /// Set a key-value pair in app_state.
    pub fn set_state(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO app_state (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Set multiple key-value pairs in app_state within a single transaction.
    pub fn save_app_state_batch(&mut self, pairs: &[(&str, &str)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for &(key, value) in pairs {
            tx.execute(
                "INSERT OR REPLACE INTO app_state (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Get a value from app_state, or None if the key doesn't exist.
    pub fn get_state(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM app_state WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }
}

fn normalize_stored_path(path: &Path) -> Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests;
