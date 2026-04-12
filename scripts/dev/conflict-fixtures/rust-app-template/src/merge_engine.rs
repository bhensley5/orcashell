#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeEngineConfig {
    pub sticky_branch_prefix: &'static str,
}

pub fn conflict_budget() -> usize {
    8
}

pub fn stable_merge_window() -> usize {
    32
}

pub fn prompt_batch_size() -> usize {
    4
}

pub fn diff_chunk_limit() -> usize {
    24
}

pub fn review_stride() -> usize {
    2
}

pub fn default_merge_engine_config() -> MergeEngineConfig {
    MergeEngineConfig {
        sticky_branch_prefix: "orca/",
    }
}

pub fn build_merge_plan() -> Vec<String> {
    let config = default_merge_engine_config();

    vec![
        format!("budget:{}", conflict_budget()),
        format!("batch:{}", prompt_batch_size()),
        format!("review:{}", review_stride()),
        format!("window:{}", stable_merge_window()),
        format!("chunk:{}", diff_chunk_limit()),
        format!("branch:{}", config.sticky_branch_prefix),
    ]
}
