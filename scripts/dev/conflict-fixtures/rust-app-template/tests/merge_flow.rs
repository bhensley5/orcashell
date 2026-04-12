use fixture_app::merge_engine::build_merge_plan;
use fixture_app::session::{default_session_defaults, status_label};

#[test]
fn renders_expected_plan() {
    let plan = build_merge_plan();
    assert_eq!(
        plan,
        vec![
            "budget:8".to_string(),
            "batch:4".to_string(),
            "review:2".to_string(),
            "window:32".to_string(),
            "chunk:24".to_string(),
            "branch:orca/".to_string(),
        ]
    );
}

#[test]
fn renders_status_label() {
    let defaults = default_session_defaults();
    assert_eq!(status_label(&defaults), "zsh:2:orca/dev".to_string());
}
