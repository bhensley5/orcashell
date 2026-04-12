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

    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM agent_terminals", [], |r| r.get(0))
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

fn make_agent_terminal(
    terminal_id: &str,
    project_id: &str,
    agent_kind: ResumableAgentKind,
    cwd: &str,
) -> StoredAgentTerminal {
    StoredAgentTerminal {
        terminal_id: terminal_id.to_string(),
        project_id: project_id.to_string(),
        agent_kind,
        cwd: PathBuf::from(cwd),
        updated_at: String::new(),
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
fn test_load_worktrees_for_repo_root() {
    let store = Store::open_in_memory().unwrap();
    store
        .save_worktree(&make_worktree(
            "wt-1",
            "p1",
            "/repo/.orcashell/worktrees/wt-1",
        ))
        .unwrap();

    let mut other_repo = make_worktree("wt-2", "p2", "/other/.orcashell/worktrees/wt-2");
    other_repo.repo_root = PathBuf::from("/other");
    other_repo.path = PathBuf::from("/other/.orcashell/worktrees/wt-2");
    store.save_worktree(&other_repo).unwrap();

    let worktrees = store
        .load_worktrees_for_repo_root(Path::new("/repo"))
        .unwrap();
    assert_eq!(worktrees.len(), 1);
    assert_eq!(worktrees[0].id, "wt-1");
    assert_eq!(worktrees[0].branch_name, "orca/wt-1");
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
fn test_upsert_load_delete_agent_terminal() {
    let store = Store::open_in_memory().unwrap();
    let row = make_agent_terminal("term-1", "p1", ResumableAgentKind::Codex, "/repo/wt-1");

    store.upsert_agent_terminal(&row).unwrap();

    let loaded = store.load_agent_terminals_for_project("p1").unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].terminal_id, "term-1");
    assert_eq!(loaded[0].project_id, "p1");
    assert_eq!(loaded[0].agent_kind, ResumableAgentKind::Codex);
    assert_eq!(loaded[0].cwd, PathBuf::from("/repo/wt-1"));

    store.delete_agent_terminal("term-1").unwrap();
    assert!(store
        .load_agent_terminals_for_project("p1")
        .unwrap()
        .is_empty());
}

#[test]
fn test_load_agent_terminals_orders_by_updated_at_then_terminal_id() {
    let store = Store::open_in_memory().unwrap();
    store
        .upsert_agent_terminal(&make_agent_terminal(
            "term-a",
            "p1",
            ResumableAgentKind::Codex,
            "/repo/wt",
        ))
        .unwrap();
    store
        .upsert_agent_terminal(&make_agent_terminal(
            "term-b",
            "p1",
            ResumableAgentKind::Codex,
            "/repo/wt",
        ))
        .unwrap();
    store
        .upsert_agent_terminal(&make_agent_terminal(
            "term-c",
            "p1",
            ResumableAgentKind::ClaudeCode,
            "/repo/wt-2",
        ))
        .unwrap();

    store
        .conn
        .execute(
            "UPDATE agent_terminals SET updated_at = ?1 WHERE terminal_id = ?2",
            params!["2026-01-01 00:00:00", "term-a"],
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE agent_terminals SET updated_at = ?1 WHERE terminal_id = ?2",
            params!["2026-01-01 00:00:00", "term-b"],
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE agent_terminals SET updated_at = ?1 WHERE terminal_id = ?2",
            params!["2026-02-01 00:00:00", "term-c"],
        )
        .unwrap();

    let loaded = store.load_agent_terminals_for_project("p1").unwrap();
    let terminal_ids: Vec<_> = loaded.iter().map(|row| row.terminal_id.as_str()).collect();
    assert_eq!(terminal_ids, vec!["term-c", "term-b", "term-a"]);
}

#[test]
fn test_delete_project_cascades_agent_terminals() {
    let store = Store::open_in_memory().unwrap();
    store
        .save_project(&make_project("p1", "First", "/a", 0))
        .unwrap();
    store
        .upsert_agent_terminal(&make_agent_terminal(
            "term-1",
            "p1",
            ResumableAgentKind::Codex,
            "/repo/wt",
        ))
        .unwrap();

    store.delete_project("p1").unwrap();
    assert!(store
        .load_agent_terminals_for_project("p1")
        .unwrap()
        .is_empty());
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
fn test_delete_window_cascades_agent_terminals() {
    let mut store = Store::open_in_memory().unwrap();
    store.save_window(&make_window(2)).unwrap();
    store
        .save_project(&make_project_for_window("p1", "A", "/a", 0, 2))
        .unwrap();
    store
        .upsert_agent_terminal(&make_agent_terminal(
            "term-1",
            "p1",
            ResumableAgentKind::ClaudeCode,
            "/repo/wt",
        ))
        .unwrap();

    store.delete_window(2).unwrap();
    assert!(store
        .load_agent_terminals_for_project("p1")
        .unwrap()
        .is_empty());
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
fn test_save_window_state_deletes_agent_rows_for_removed_projects() {
    let mut store = Store::open_in_memory().unwrap();
    let w = make_window(1);
    store
        .save_window_state(
            &w,
            &[
                make_project("p1", "First", "/a", 0),
                make_project("p2", "Second", "/b", 1),
            ],
        )
        .unwrap();
    store
        .upsert_agent_terminal(&make_agent_terminal(
            "term-1",
            "p2",
            ResumableAgentKind::Codex,
            "/repo/wt",
        ))
        .unwrap();

    store
        .save_window_state(&w, &[make_project("p1", "First", "/a", 0)])
        .unwrap();

    assert!(store
        .load_agent_terminals_for_project("p2")
        .unwrap()
        .is_empty());
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
