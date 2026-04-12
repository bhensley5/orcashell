#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDefaults {
    pub shell: &'static str,
    pub retry_budget: u8,
    pub idle_timeout_ms: u64,
    pub prompt_tag: &'static str,
    pub review_requests_enabled: bool,
}

pub fn default_session_defaults() -> SessionDefaults {
    SessionDefaults {
        shell: "zsh",
        retry_budget: 2,
        idle_timeout_ms: 1_200,
        prompt_tag: "orca/dev",
        review_requests_enabled: true,
    }
}

pub fn status_label(defaults: &SessionDefaults) -> String {
    format!(
        "{}:{}:{}",
        defaults.shell, defaults.retry_budget, defaults.prompt_tag
    )
}
