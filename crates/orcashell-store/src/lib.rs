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
            );",
        )?;
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_worktrees_project_id
                ON worktrees(project_id);

            CREATE INDEX IF NOT EXISTS idx_worktrees_primary_terminal_id
                ON worktrees(primary_terminal_id);",
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
        tx.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
        tx.commit()?;
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
mod tests {
    use super::*;

    #[test]
    fn test_open_creates_schema() {
        let store = Store::open_in_memory().unwrap();
        // Verify tables exist by querying them
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM app_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM worktrees", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    fn make_project(id: &str, name: &str, path: &str, order: i32) -> StoredProject {
        make_project_for_window(id, name, path, order, 1)
    }

    fn make_project_for_window(
        id: &str,
        name: &str,
        path: &str,
        order: i32,
        window_id: i64,
    ) -> StoredProject {
        StoredProject {
            id: id.to_string(),
            name: name.to_string(),
            path: PathBuf::from(path),
            layout_json: r#"{"Terminal":{"terminal_id":null}}"#.to_string(),
            terminal_names_json: "{}".to_string(),
            sort_order: order,
            window_id,
        }
    }

    fn make_window(window_id: i64) -> StoredWindow {
        StoredWindow {
            window_id,
            bounds_x: Some(100.0),
            bounds_y: Some(200.0),
            bounds_width: 1200.0,
            bounds_height: 800.0,
            active_project_id: None,
            sort_order: window_id as i32 - 1,
            is_open: true,
        }
    }

    fn make_worktree(id: &str, project_id: &str, path: &str) -> StoredWorktree {
        StoredWorktree {
            id: id.to_string(),
            project_id: project_id.to_string(),
            repo_root: PathBuf::from("/repo"),
            path: PathBuf::from(path),
            worktree_name: id.to_string(),
            branch_name: format!("orca/{id}"),
            source_ref: "refs/heads/main".to_string(),
            primary_terminal_id: Some(format!("term-{id}")),
        }
    }

    #[test]
    fn test_save_load_project() {
        let store = Store::open_in_memory().unwrap();
        let project = make_project("p1", "My Project", "/home/user/project", 0);
        store.save_project(&project).unwrap();

        let loaded = store.load_projects().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "p1");
        assert_eq!(loaded[0].name, "My Project");
        assert_eq!(loaded[0].path, PathBuf::from("/home/user/project"));
        assert_eq!(loaded[0].sort_order, 0);
    }

    #[test]
    fn test_save_load_multiple_projects() {
        let store = Store::open_in_memory().unwrap();
        store
            .save_project(&make_project("p1", "First", "/a", 2))
            .unwrap();
        store
            .save_project(&make_project("p2", "Second", "/b", 0))
            .unwrap();
        store
            .save_project(&make_project("p3", "Third", "/c", 1))
            .unwrap();

        let loaded = store.load_projects().unwrap();
        assert_eq!(loaded.len(), 3);
        // Should be ordered by sort_order
        assert_eq!(loaded[0].id, "p2");
        assert_eq!(loaded[1].id, "p3");
        assert_eq!(loaded[2].id, "p1");
    }

    #[test]
    fn test_delete_project() {
        let store = Store::open_in_memory().unwrap();
        store
            .save_project(&make_project("p1", "First", "/a", 0))
            .unwrap();
        store
            .save_project(&make_project("p2", "Second", "/b", 1))
            .unwrap();

        store.delete_project("p1").unwrap();
        let loaded = store.load_projects().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "p2");
    }

    #[test]
    fn test_save_load_find_worktree() {
        let tempdir = tempfile::tempdir().unwrap();
        let repo_root = tempdir.path().join("repo");
        let worktree_path = repo_root.join(".orcashell/worktrees/wt-1");
        std::fs::create_dir_all(&worktree_path).unwrap();
        let store = Store::open_in_memory().unwrap();
        store
            .save_worktree(&StoredWorktree {
                repo_root: repo_root.clone(),
                path: worktree_path.clone(),
                ..make_worktree("wt-1", "p1", "/repo/.orcashell/worktrees/wt-1")
            })
            .unwrap();

        let worktrees = store.load_worktrees_for_project("p1").unwrap();
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].id, "wt-1");
        assert_eq!(worktrees[0].branch_name, "orca/wt-1");
        assert_eq!(worktrees[0].repo_root, repo_root.canonicalize().unwrap());
        assert_eq!(worktrees[0].path, worktree_path.canonicalize().unwrap());

        let found = store
            .find_worktree_by_path(&worktree_path)
            .unwrap()
            .unwrap();
        assert_eq!(found.id, "wt-1");
        assert_eq!(found.project_id, "p1");
    }

    #[test]
    fn test_save_worktree_overwrites_existing_row() {
        let store = Store::open_in_memory().unwrap();
        let mut worktree = make_worktree("wt-1", "p1", "/repo/.orcashell/worktrees/wt-1");
        store.save_worktree(&worktree).unwrap();

        worktree.primary_terminal_id = None;
        worktree.source_ref = "refs/heads/release".into();
        store.save_worktree(&worktree).unwrap();

        let found = store
            .find_worktree_by_path(Path::new("/repo/.orcashell/worktrees/wt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(found.source_ref, "refs/heads/release");
        assert_eq!(found.primary_terminal_id, None);
    }

    #[test]
    fn test_delete_project_cascades_worktrees() {
        let store = Store::open_in_memory().unwrap();
        store
            .save_project(&make_project("p1", "First", "/a", 0))
            .unwrap();
        store
            .save_worktree(&make_worktree(
                "wt-1",
                "p1",
                "/repo/.orcashell/worktrees/wt-1",
            ))
            .unwrap();

        store.delete_project("p1").unwrap();
        assert!(store.load_worktrees_for_project("p1").unwrap().is_empty());
    }

    #[test]
    fn test_update_project_order() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .save_project(&make_project("p1", "First", "/a", 0))
            .unwrap();
        store
            .save_project(&make_project("p2", "Second", "/b", 1))
            .unwrap();
        store
            .save_project(&make_project("p3", "Third", "/c", 2))
            .unwrap();

        // Reverse order
        store.update_project_order(&["p3", "p2", "p1"]).unwrap();

        let loaded = store.load_projects().unwrap();
        assert_eq!(loaded[0].id, "p3");
        assert_eq!(loaded[1].id, "p2");
        assert_eq!(loaded[2].id, "p1");
    }

    #[test]
    fn test_set_get_state() {
        let store = Store::open_in_memory().unwrap();
        store.set_state("active_project_id", "p1").unwrap();
        let val = store.get_state("active_project_id").unwrap();
        assert_eq!(val, Some("p1".to_string()));
    }

    #[test]
    fn test_get_missing_state() {
        let store = Store::open_in_memory().unwrap();
        let val = store.get_state("nonexistent_key").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn test_save_project_overwrites() {
        let store = Store::open_in_memory().unwrap();
        store
            .save_project(&make_project("p1", "Original", "/a", 0))
            .unwrap();
        store
            .save_project(&make_project("p1", "Updated", "/b", 1))
            .unwrap();

        let loaded = store.load_projects().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Updated");
        assert_eq!(loaded[0].path, PathBuf::from("/b"));
    }

    // ── Window tests ──

    #[test]
    fn test_schema_creates_windows_table() {
        let store = Store::open_in_memory().unwrap();
        // Fresh DB has no windows
        let windows = store.load_windows().unwrap();
        assert_eq!(windows.len(), 0);
    }

    #[test]
    fn test_save_load_window() {
        let store = Store::open_in_memory().unwrap();
        let mut w = make_window(2);
        w.active_project_id = Some("p1".to_string());
        store.save_window(&w).unwrap();

        let windows = store.load_windows().unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].window_id, 2);
        assert_eq!(windows[0].bounds_x, Some(100.0));
        assert_eq!(windows[0].bounds_y, Some(200.0));
        assert_eq!(windows[0].bounds_width, 1200.0);
        assert_eq!(windows[0].bounds_height, 800.0);
        assert_eq!(windows[0].active_project_id, Some("p1".to_string()));
    }

    #[test]
    fn test_next_window_id() {
        let store = Store::open_in_memory().unwrap();
        // Empty DB
        assert_eq!(store.next_window_id().unwrap(), 1);

        store.save_window(&make_window(1)).unwrap();
        assert_eq!(store.next_window_id().unwrap(), 2);

        store.save_window(&make_window(5)).unwrap();
        assert_eq!(store.next_window_id().unwrap(), 6);
    }

    #[test]
    fn test_delete_window_cascades_projects() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_window(&make_window(1)).unwrap();
        store.save_window(&make_window(2)).unwrap();
        store
            .save_project(&make_project_for_window("p1", "A", "/a", 0, 2))
            .unwrap();
        store
            .save_project(&make_project_for_window("p2", "B", "/b", 1, 2))
            .unwrap();
        store
            .save_project(&make_project("p3", "C", "/c", 0))
            .unwrap(); // window 1

        store.delete_window(2).unwrap();

        let windows = store.load_windows().unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].window_id, 1);

        let projects = store.load_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, "p3");
    }

    #[test]
    fn test_delete_window_cascades_worktrees() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_window(&make_window(2)).unwrap();
        store
            .save_project(&make_project_for_window("p1", "A", "/a", 0, 2))
            .unwrap();
        store
            .save_worktree(&make_worktree(
                "wt-1",
                "p1",
                "/repo/.orcashell/worktrees/wt-1",
            ))
            .unwrap();

        store.delete_window(2).unwrap();
        assert!(store.load_worktrees_for_project("p1").unwrap().is_empty());
    }

    #[test]
    fn test_load_projects_for_window() {
        let store = Store::open_in_memory().unwrap();
        store.save_window(&make_window(2)).unwrap();
        store
            .save_project(&make_project("p1", "Win1", "/a", 0))
            .unwrap();
        store
            .save_project(&make_project_for_window("p2", "Win2", "/b", 0, 2))
            .unwrap();
        store
            .save_project(&make_project_for_window("p3", "Win2b", "/c", 1, 2))
            .unwrap();

        let w1 = store.load_projects_for_window(1).unwrap();
        assert_eq!(w1.len(), 1);
        assert_eq!(w1[0].id, "p1");

        let w2 = store.load_projects_for_window(2).unwrap();
        assert_eq!(w2.len(), 2);
        assert_eq!(w2[0].id, "p2");
        assert_eq!(w2[1].id, "p3");
    }

    #[test]
    fn test_save_window_state_transaction() {
        let mut store = Store::open_in_memory().unwrap();
        let mut w = make_window(1);
        w.active_project_id = Some("p1".to_string());

        let projects = vec![
            make_project("p1", "First", "/a", 0),
            make_project("p2", "Second", "/b", 1),
        ];
        store.save_window_state(&w, &projects).unwrap();
        store
            .save_worktree(&make_worktree(
                "wt-1",
                "p2",
                "/repo/.orcashell/worktrees/wt-1",
            ))
            .unwrap();

        let loaded_w = store.load_windows().unwrap();
        assert_eq!(loaded_w[0].active_project_id, Some("p1".to_string()));

        let loaded_p = store.load_projects_for_window(1).unwrap();
        assert_eq!(loaded_p.len(), 2);

        // Now save with only p1. p2 should be deleted.
        store
            .save_window_state(&w, &[make_project("p1", "First", "/a", 0)])
            .unwrap();
        let loaded_p = store.load_projects_for_window(1).unwrap();
        assert_eq!(loaded_p.len(), 1);
        assert_eq!(loaded_p[0].id, "p1");
        assert!(store.load_worktrees_for_project("p2").unwrap().is_empty());
    }

    #[test]
    fn test_multiple_windows_isolation() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_window(&make_window(2)).unwrap();
        store.save_window(&make_window(3)).unwrap();

        let p1 = vec![make_project("p1", "A", "/a", 0)];
        let p2 = vec![make_project_for_window("p2", "B", "/b", 0, 2)];
        let p3 = vec![
            make_project_for_window("p3", "C", "/c", 0, 3),
            make_project_for_window("p4", "D", "/d", 1, 3),
        ];

        store.save_window_state(&make_window(1), &p1).unwrap();
        store.save_window_state(&make_window(2), &p2).unwrap();
        store.save_window_state(&make_window(3), &p3).unwrap();

        assert_eq!(store.load_projects_for_window(1).unwrap().len(), 1);
        assert_eq!(store.load_projects_for_window(2).unwrap().len(), 1);
        assert_eq!(store.load_projects_for_window(3).unwrap().len(), 2);

        // Total across all windows
        assert_eq!(store.load_projects().unwrap().len(), 4);
    }

    #[test]
    fn test_hibernate_and_restore_window() {
        let store = Store::open_in_memory().unwrap();
        store.save_window(&make_window(1)).unwrap();
        store.save_window(&make_window(2)).unwrap();
        store
            .save_project(&make_project_for_window("p1", "A", "/a", 0, 2))
            .unwrap();

        // Both windows are open
        assert_eq!(store.load_windows().unwrap().len(), 2);
        assert!(store.load_hibernated_window().unwrap().is_none());

        // Hibernate window 2
        store.hibernate_window(2).unwrap();

        // Only window 1 is "open"
        let open = store.load_windows().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].window_id, 1);

        // Window 2 is hibernated and can be restored
        let hibernated = store.load_hibernated_window().unwrap();
        assert!(hibernated.is_some());
        assert_eq!(hibernated.unwrap().window_id, 2);

        // Its projects are still there
        let projects = store.load_projects_for_window(2).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, "p1");

        // "Reopen" by marking it open again
        let mut w2 = make_window(2);
        w2.is_open = true;
        store.save_window(&w2).unwrap();
        assert_eq!(store.load_windows().unwrap().len(), 2);
        assert!(store.load_hibernated_window().unwrap().is_none());
    }

    #[test]
    fn delete_worktree_removes_row() {
        let store = Store::open_in_memory().unwrap();
        store.save_window(&make_window(1)).unwrap();
        store
            .save_project(&make_project("p1", "Proj", "/proj", 0))
            .unwrap();
        let wt = make_worktree("wt-1", "p1", "/repo/.orcashell/worktrees/wt-1");
        store.save_worktree(&wt).unwrap();

        assert!(store
            .find_worktree_by_path(Path::new("/repo/.orcashell/worktrees/wt-1"))
            .unwrap()
            .is_some());

        store.delete_worktree("wt-1").unwrap();

        assert!(store
            .find_worktree_by_path(Path::new("/repo/.orcashell/worktrees/wt-1"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn delete_worktree_nonexistent_is_noop() {
        let store = Store::open_in_memory().unwrap();
        // Should not error when deleting a non-existent worktree.
        store.delete_worktree("does-not-exist").unwrap();
    }
}
