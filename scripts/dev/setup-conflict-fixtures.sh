#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE_DIR="${BASE_DIR:-/tmp/orcashell-conflicts}"
TEMPLATE_DIR="$SCRIPT_DIR/conflict-fixtures/rust-app-template"
WORKTREE_ID="wt-ab123456"
WORKTREE_BRANCH="orca/$WORKTREE_ID"

SCENARIOS=(
  pull-basic
  pull-multi
  external-merge
  merge-back
)

main() {
  local command="${1:-all}"

  case "$command" in
    all)
      require_template_dir
      reset_base_dir
      local scenario
      for scenario in "${SCENARIOS[@]}"; do
        setup_scenario "$scenario"
      done
      print_all_summary
      ;;
    list)
      list_scenarios
      ;;
    describe)
      require_scenario_arg "${2:-}"
      describe_scenario "$2"
      ;;
    setup)
      require_template_dir
      require_scenario_arg "${2:-}"
      mkdir -p "$BASE_DIR"
      reset_scenario_dir "$2"
      setup_scenario "$2"
      print_setup_summary "$2"
      ;;
    clean)
      rm -rf "$BASE_DIR"
      printf 'Removed %s\n' "$BASE_DIR"
      ;;
    help|-h|--help)
      usage
      ;;
    pull-basic|pull-multi|external-merge|merge-back)
      require_template_dir
      mkdir -p "$BASE_DIR"
      reset_scenario_dir "$command"
      setup_scenario "$command"
      print_setup_summary "$command"
      ;;
    *)
      usage
      exit 1
      ;;
  esac
}

usage() {
  cat <<EOF
Usage:
  scripts/dev/setup-conflict-fixtures.sh all
  scripts/dev/setup-conflict-fixtures.sh list
  scripts/dev/setup-conflict-fixtures.sh describe <scenario>
  scripts/dev/setup-conflict-fixtures.sh setup <scenario>
  scripts/dev/setup-conflict-fixtures.sh clean

Scenarios:
  pull-basic
  pull-multi
  external-merge
  merge-back

Backwards-compatible shortcuts:
  scripts/dev/setup-conflict-fixtures.sh pull-basic
  scripts/dev/setup-conflict-fixtures.sh pull-multi
  scripts/dev/setup-conflict-fixtures.sh external-merge
  scripts/dev/setup-conflict-fixtures.sh merge-back

Options:
  BASE_DIR=/tmp/orcashell-conflicts  Override fixture root directory.

Examples:
  scripts/dev/setup-conflict-fixtures.sh list
  scripts/dev/setup-conflict-fixtures.sh describe pull-multi
  scripts/dev/setup-conflict-fixtures.sh setup merge-back
  BASE_DIR=/tmp/orca-fixtures scripts/dev/setup-conflict-fixtures.sh all
EOF
}

require_template_dir() {
  if [[ ! -d "$TEMPLATE_DIR" ]]; then
    printf 'Missing template directory: %s\n' "$TEMPLATE_DIR" >&2
    exit 1
  fi
}

require_scenario_arg() {
  local scenario="${1:-}"
  if ! is_valid_scenario "$scenario"; then
    printf 'Unknown or missing scenario. Use one of: %s\n' "${SCENARIOS[*]}" >&2
    exit 1
  fi
}

is_valid_scenario() {
  local candidate="${1:-}"
  local scenario
  for scenario in "${SCENARIOS[@]}"; do
    if [[ "$scenario" == "$candidate" ]]; then
      return 0
    fi
  done
  return 1
}

reset_base_dir() {
  rm -rf "$BASE_DIR"
  mkdir -p "$BASE_DIR"
}

reset_scenario_dir() {
  local scenario="$1"
  rm -rf "$(scenario_dir "$scenario")"
}

scenario_dir() {
  printf '%s/%s' "$BASE_DIR" "$1"
}

scenario_title() {
  case "$1" in
    pull-basic) printf 'Pull Conflict: Single Code File + Clean Auto-Merges' ;;
    pull-multi) printf 'Pull Conflict: Multi-Block + Multi-File diff3' ;;
    external-merge) printf 'External Merge Resume: Partially Resolved diff3' ;;
    merge-back) printf 'Managed Worktree Merge-Back: Source Handoff' ;;
  esac
}

scenario_summary() {
  case "$1" in
    pull-basic) printf 'Run Pull to enter a realistic single-file conflict with clean merged neighbors.' ;;
    pull-multi) printf 'Run Pull to get multiple conflict blocks, a second conflicted file, and diff3 base sections.' ;;
    external-merge) printf 'Open an already-conflicted repo with one file resolved, staged, and ready to resume.' ;;
    merge-back) printf 'Run Merge from a managed worktree and resolve the resulting conflicts in the source repo.' ;;
  esac
}

scenario_entry_action() {
  case "$1" in
    pull-basic|pull-multi) printf 'pull' ;;
    external-merge) printf 'open_mid_merge' ;;
    merge-back) printf 'merge_back' ;;
  esac
}

scenario_conflict_style() {
  case "$1" in
    pull-basic|merge-back) printf 'standard' ;;
    pull-multi|external-merge) printf 'diff3' ;;
  esac
}

scenario_open_paths() {
  local scenario="$1"
  local root
  root="$(scenario_dir "$scenario")"

  case "$scenario" in
    pull-basic|pull-multi|external-merge)
      printf '%s/mine\n' "$root"
      ;;
    merge-back)
      printf '%s/repo\n' "$root"
      printf '%s/repo/.orcashell/worktrees/%s\n' "$root" "$WORKTREE_ID"
      ;;
  esac
}

scenario_expected_conflicts() {
  case "$1" in
    pull-basic)
      printf 'src/session.rs\n'
      ;;
    pull-multi)
      printf 'src/merge_engine.rs\n'
      printf 'tests/merge_flow.rs\n'
      ;;
    external-merge)
      printf 'src/merge_engine.rs\n'
      ;;
    merge-back)
      printf 'src/session.rs\n'
      printf 'tests/merge_flow.rs\n'
      ;;
  esac
}

scenario_expected_staged() {
  case "$1" in
    pull-basic)
      printf 'config/dev.json\n'
      printf 'README.md\n'
      ;;
    pull-multi)
      printf 'config/dev.json\n'
      printf 'README.md\n'
      ;;
    external-merge)
      printf 'config/dev.json\n'
      printf 'README.md\n'
      printf 'src/session.rs\n'
      ;;
    merge-back)
      printf 'README.md\n'
      printf 'config/dev.json\n'
      ;;
  esac
}

scenario_notes() {
  case "$1" in
    pull-basic)
      printf 'Use this to validate baseline conflict entry, file tree ordering, and a single-block resolution flow.\n'
      printf 'Only one file should remain in the Conflicts section after Pull; clean merges should already be staged.\n'
      ;;
    pull-multi)
      printf 'This scenario enables diff3 markers so the conflict editor should expose Base sections and Accept Base.\n'
      printf 'The main conflicted file contains three separated conflict blocks for Prev/Next validation.\n'
      ;;
    external-merge)
      printf 'The repo is already mid-merge when generated, so opening OrcaShell should resume the conflict workflow immediately.\n'
      printf 'One conflicted file has already been resolved and staged to prove mixed conflict plus staged state during resume.\n'
      ;;
    merge-back)
      printf 'Open both the source repo and the managed worktree path, then run Merge from the worktree diff tab.\n'
      printf 'Conflicts should land only in the source repo; the worktree should stay clean after the merge attempt.\n'
      ;;
  esac
}

entry_action_text() {
  case "$1" in
    pull) printf 'Open the repo at the mine path and run Pull in OrcaShell.' ;;
    open_mid_merge) printf 'Open the repo path directly; it is already in a real merge state.' ;;
    merge_back) printf 'Open both listed paths, then run Merge from the managed worktree diff tab.' ;;
  esac
}

list_scenarios() {
  local scenario
  for scenario in "${SCENARIOS[@]}"; do
    printf '%-15s %s\n' "$scenario" "$(scenario_summary "$scenario")"
  done
}

describe_scenario() {
  local scenario="$1"
  local root
  root="$(scenario_dir "$scenario")"

  printf 'Scenario: %s\n' "$scenario"
  printf 'Title:    %s\n' "$(scenario_title "$scenario")"
  printf 'Summary:  %s\n' "$(scenario_summary "$scenario")"
  printf 'Root:     %s\n' "$root"
  printf 'Action:   %s\n' "$(entry_action_text "$(scenario_entry_action "$scenario")")"
  printf 'Markers:  %s\n' "$(scenario_conflict_style "$scenario")"
  print_labeled_list 'Open paths' "$(scenario_open_paths "$scenario")"
  print_labeled_list 'Expected conflicted files' "$(scenario_expected_conflicts "$scenario")"
  print_labeled_list 'Expected staged files after entry' "$(scenario_expected_staged "$scenario")"
  print_labeled_list 'Notes' "$(scenario_notes "$scenario")"
}

print_labeled_list() {
  local label="$1"
  local lines="$2"
  local first=1

  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    if (( first )); then
      printf '%s:\n' "$label"
      first=0
    fi
    printf '  - %s\n' "$line"
  done <<< "$lines"
}

init_repo() {
  local repo_dir="$1"
  mkdir -p "$repo_dir"
  git init -b main "$repo_dir" >/dev/null 2>&1
  git -C "$repo_dir" config user.name "OrcaShell Fixture"
  git -C "$repo_dir" config user.email "fixture@orcashell.local"
}

init_clone() {
  local origin_dir="$1"
  local clone_dir="$2"
  git clone "$origin_dir" "$clone_dir" >/dev/null 2>&1
  git -C "$clone_dir" config user.name "OrcaShell Fixture"
  git -C "$clone_dir" config user.email "fixture@orcashell.local"
}

ensure_orcashell_exclude() {
  local repo_dir="$1"
  local exclude_file="$repo_dir/.git/info/exclude"
  mkdir -p "$(dirname "$exclude_file")"
  touch "$exclude_file"
  if ! grep -qxF '/.orcashell/' "$exclude_file"; then
    printf '/.orcashell/\n' >>"$exclude_file"
  fi
}

materialize_template() {
  local repo_dir="$1"
  cp -R "$TEMPLATE_DIR"/. "$repo_dir"/
}

write_file() {
  local path="$1"
  mkdir -p "$(dirname "$path")"
  cat >"$path"
}

commit_all() {
  local repo_dir="$1"
  local message="$2"
  git -C "$repo_dir" add -A
  git -C "$repo_dir" commit -m "$message" >/dev/null
}

create_pull_scenario_base() {
  local scenario="$1"
  local root
  local origin_dir
  local mine_dir
  local remote_dir

  root="$(scenario_dir "$scenario")"
  origin_dir="$root/origin.git"
  mine_dir="$root/mine"
  remote_dir="$root/remote"

  mkdir -p "$root"
  git init --bare "$origin_dir" >/dev/null 2>&1
  init_repo "$mine_dir"
  git -C "$mine_dir" remote add origin "$origin_dir"
  materialize_template "$mine_dir"
  commit_all "$mine_dir" "initial fixture"
  git -C "$mine_dir" push -u origin main >/dev/null 2>&1
  init_clone "$origin_dir" "$remote_dir"
}

ensure_merge_state() {
  local repo_dir="$1"
  local scenario="$2"
  if [[ ! -f "$repo_dir/.git/MERGE_HEAD" ]]; then
    printf 'Scenario %s did not enter merge state at %s\n' "$scenario" "$repo_dir" >&2
    exit 1
  fi
}

setup_scenario() {
  case "$1" in
    pull-basic) setup_pull_basic ;;
    pull-multi) setup_pull_multi ;;
    external-merge) setup_external_merge ;;
    merge-back) setup_merge_back ;;
  esac
}

setup_pull_basic() {
  local root
  local mine_dir
  local remote_dir

  create_pull_scenario_base pull-basic
  root="$(scenario_dir pull-basic)"
  mine_dir="$root/mine"
  remote_dir="$root/remote"

  write_file "$remote_dir/src/session.rs" <<'EOF'
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
        retry_budget: 4,
        idle_timeout_ms: 1_200,
        prompt_tag: "orca/remote",
        review_requests_enabled: true,
    }
}

pub fn status_label(defaults: &SessionDefaults) -> String {
    format!(
        "{}:{}:{}",
        defaults.shell, defaults.retry_budget, defaults.prompt_tag
    )
}
EOF
  write_file "$remote_dir/config/dev.json" <<'EOF'
{
  "project": "fixture-app",
  "shell": "zsh",
  "prompt_tag": "orca/dev",
  "idle_timeout_ms": 900,
  "review_batch_size": 4,
  "merge_strategy": "balanced"
}
EOF
  write_file "$remote_dir/README.md" <<'EOF'
# Fixture App

Fixture App is a tiny local-only repo used to exercise OrcaShell merge tooling.

## Workflow

1. Keep terminal-first experiments in dedicated worktrees.
2. Review merges in OrcaShell before landing them.
3. Keep small release notes close to the code.
EOF
  commit_all "$remote_dir" "remote fixture updates"
  git -C "$remote_dir" push >/dev/null 2>&1
  git -C "$mine_dir" fetch origin >/dev/null 2>&1

  write_file "$mine_dir/src/session.rs" <<'EOF'
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
        retry_budget: 6,
        idle_timeout_ms: 1_200,
        prompt_tag: "orca/local",
        review_requests_enabled: true,
    }
}

pub fn status_label(defaults: &SessionDefaults) -> String {
    format!(
        "{}:{}:{}",
        defaults.shell, defaults.retry_budget, defaults.prompt_tag
    )
}
EOF
  commit_all "$mine_dir" "local fixture updates"

  write_scenario_metadata pull-basic
}

setup_pull_multi() {
  local root
  local mine_dir
  local remote_dir

  create_pull_scenario_base pull-multi
  root="$(scenario_dir pull-multi)"
  mine_dir="$root/mine"
  remote_dir="$root/remote"

  git -C "$mine_dir" config merge.conflictstyle diff3

  write_file "$remote_dir/src/merge_engine.rs" <<'EOF'
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeEngineConfig {
    pub sticky_branch_prefix: &'static str,
}

pub fn conflict_budget() -> usize {
    12
}

pub fn stable_merge_window() -> usize {
    32
}

pub fn prompt_batch_size() -> usize {
    6
}

pub fn diff_chunk_limit() -> usize {
    24
}

pub fn review_stride() -> usize {
    3
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
EOF
  write_file "$remote_dir/tests/merge_flow.rs" <<'EOF'
use fixture_app::merge_engine::build_merge_plan;

#[test]
fn renders_expected_plan() {
    let plan = build_merge_plan();
    assert_eq!(
        plan,
        vec![
            "budget:12".to_string(),
            "batch:6".to_string(),
            "review:3".to_string(),
            "window:32".to_string(),
            "chunk:24".to_string(),
            "branch:orca/".to_string(),
        ]
    );
}
EOF
  write_file "$remote_dir/config/dev.json" <<'EOF'
{
  "project": "fixture-app",
  "shell": "zsh",
  "prompt_tag": "orca/dev",
  "idle_timeout_ms": 900,
  "review_batch_size": 4,
  "merge_strategy": "balanced"
}
EOF
  write_file "$remote_dir/README.md" <<'EOF'
# Fixture App

Fixture App is a tiny local-only repo used to exercise OrcaShell merge tooling.

## Workflow

1. Keep terminal-first experiments in dedicated worktrees.
2. Review merges in OrcaShell before landing them.
3. Keep small release notes close to the code.
EOF
  commit_all "$remote_dir" "remote diff3 updates"
  git -C "$remote_dir" push >/dev/null 2>&1
  git -C "$mine_dir" fetch origin >/dev/null 2>&1

  write_file "$mine_dir/src/merge_engine.rs" <<'EOF'
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeEngineConfig {
    pub sticky_branch_prefix: &'static str,
}

pub fn conflict_budget() -> usize {
    10
}

pub fn stable_merge_window() -> usize {
    32
}

pub fn prompt_batch_size() -> usize {
    8
}

pub fn diff_chunk_limit() -> usize {
    24
}

pub fn review_stride() -> usize {
    5
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
EOF
  write_file "$mine_dir/tests/merge_flow.rs" <<'EOF'
use fixture_app::merge_engine::build_merge_plan;

#[test]
fn renders_expected_plan() {
    let plan = build_merge_plan();
    assert_eq!(
        plan,
        vec![
            "budget:10".to_string(),
            "batch:8".to_string(),
            "review:5".to_string(),
            "window:32".to_string(),
            "chunk:24".to_string(),
            "branch:orca/".to_string(),
        ]
    );
}
EOF
  commit_all "$mine_dir" "local diff3 updates"

  write_scenario_metadata pull-multi
}

setup_external_merge() {
  local root
  local mine_dir
  local remote_dir

  create_pull_scenario_base external-merge
  root="$(scenario_dir external-merge)"
  mine_dir="$root/mine"
  remote_dir="$root/remote"

  git -C "$mine_dir" config merge.conflictstyle diff3

  write_file "$remote_dir/src/session.rs" <<'EOF'
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
        retry_budget: 4,
        idle_timeout_ms: 1_200,
        prompt_tag: "orca/remote",
        review_requests_enabled: true,
    }
}

pub fn status_label(defaults: &SessionDefaults) -> String {
    format!(
        "{}:{}:{}",
        defaults.shell, defaults.retry_budget, defaults.prompt_tag
    )
}
EOF
  write_file "$remote_dir/src/merge_engine.rs" <<'EOF'
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeEngineConfig {
    pub sticky_branch_prefix: &'static str,
}

pub fn conflict_budget() -> usize {
    12
}

pub fn stable_merge_window() -> usize {
    32
}

pub fn prompt_batch_size() -> usize {
    6
}

pub fn diff_chunk_limit() -> usize {
    24
}

pub fn review_stride() -> usize {
    3
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
EOF
  write_file "$remote_dir/config/dev.json" <<'EOF'
{
  "project": "fixture-app",
  "shell": "zsh",
  "prompt_tag": "orca/dev",
  "idle_timeout_ms": 900,
  "review_batch_size": 4,
  "merge_strategy": "balanced"
}
EOF
  write_file "$remote_dir/README.md" <<'EOF'
# Fixture App

Fixture App is a tiny local-only repo used to exercise OrcaShell merge tooling.

## Workflow

1. Keep terminal-first experiments in dedicated worktrees.
2. Review merges in OrcaShell before landing them.
3. Keep small release notes close to the code.
EOF
  commit_all "$remote_dir" "remote resume updates"
  git -C "$remote_dir" push >/dev/null 2>&1

  write_file "$mine_dir/src/session.rs" <<'EOF'
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
        retry_budget: 6,
        idle_timeout_ms: 1_200,
        prompt_tag: "orca/local",
        review_requests_enabled: true,
    }
}

pub fn status_label(defaults: &SessionDefaults) -> String {
    format!(
        "{}:{}:{}",
        defaults.shell, defaults.retry_budget, defaults.prompt_tag
    )
}
EOF
  write_file "$mine_dir/src/merge_engine.rs" <<'EOF'
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeEngineConfig {
    pub sticky_branch_prefix: &'static str,
}

pub fn conflict_budget() -> usize {
    10
}

pub fn stable_merge_window() -> usize {
    32
}

pub fn prompt_batch_size() -> usize {
    8
}

pub fn diff_chunk_limit() -> usize {
    24
}

pub fn review_stride() -> usize {
    5
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
EOF
  commit_all "$mine_dir" "local resume updates"

  git -C "$mine_dir" pull --no-rebase >/dev/null 2>&1 || true
  ensure_merge_state "$mine_dir" external-merge

  write_file "$mine_dir/src/session.rs" <<'EOF'
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
        retry_budget: 6,
        idle_timeout_ms: 1_200,
        prompt_tag: "orca/local+remote",
        review_requests_enabled: true,
    }
}

pub fn status_label(defaults: &SessionDefaults) -> String {
    format!(
        "{}:{}:{}",
        defaults.shell, defaults.retry_budget, defaults.prompt_tag
    )
}
EOF
  git -C "$mine_dir" add src/session.rs

  write_scenario_metadata external-merge
}

setup_merge_back() {
  local root
  local repo_dir
  local worktree_dir

  root="$(scenario_dir merge-back)"
  repo_dir="$root/repo"
  worktree_dir="$repo_dir/.orcashell/worktrees/$WORKTREE_ID"

  init_repo "$repo_dir"
  ensure_orcashell_exclude "$repo_dir"
  materialize_template "$repo_dir"
  commit_all "$repo_dir" "initial fixture"

  git -C "$repo_dir" branch "$WORKTREE_BRANCH" >/dev/null
  mkdir -p "$repo_dir/.orcashell/worktrees"
  git -C "$repo_dir" worktree add "$worktree_dir" "$WORKTREE_BRANCH" >/dev/null 2>&1
  git -C "$worktree_dir" config user.name "OrcaShell Fixture"
  git -C "$worktree_dir" config user.email "fixture@orcashell.local"

  write_file "$worktree_dir/src/session.rs" <<'EOF'
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
        retry_budget: 4,
        idle_timeout_ms: 1_200,
        prompt_tag: "orca/worktree",
        review_requests_enabled: true,
    }
}

pub fn status_label(defaults: &SessionDefaults) -> String {
    format!(
        "{}:{}:{}",
        defaults.shell, defaults.retry_budget, defaults.prompt_tag
    )
}
EOF
  write_file "$worktree_dir/tests/merge_flow.rs" <<'EOF'
use fixture_app::session::{default_session_defaults, status_label};

#[test]
fn renders_status_label() {
    let defaults = default_session_defaults();
    assert_eq!(status_label(&defaults), "zsh:4:orca/worktree".to_string());
}
EOF
  write_file "$worktree_dir/config/dev.json" <<'EOF'
{
  "project": "fixture-app",
  "shell": "zsh",
  "prompt_tag": "orca/worktree",
  "idle_timeout_ms": 1_200,
  "review_batch_size": 6,
  "merge_strategy": "balanced"
}
EOF
  write_file "$worktree_dir/README.md" <<'EOF'
# Fixture App

Fixture App is a tiny local-only repo used to exercise OrcaShell merge tooling.

## Workflow

1. Create managed worktrees for risky experiments.
2. Review merges in OrcaShell before landing them.
3. Keep worktree notes close to merge experiments.
EOF
  commit_all "$worktree_dir" "worktree branch updates"

  write_file "$repo_dir/src/session.rs" <<'EOF'
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
        retry_budget: 6,
        idle_timeout_ms: 1_200,
        prompt_tag: "orca/source",
        review_requests_enabled: true,
    }
}

pub fn status_label(defaults: &SessionDefaults) -> String {
    format!(
        "{}:{}:{}",
        defaults.shell, defaults.retry_budget, defaults.prompt_tag
    )
}
EOF
  write_file "$repo_dir/tests/merge_flow.rs" <<'EOF'
use fixture_app::session::{default_session_defaults, status_label};

#[test]
fn renders_status_label() {
    let defaults = default_session_defaults();
    assert_eq!(status_label(&defaults), "zsh:6:orca/source".to_string());
}
EOF
  commit_all "$repo_dir" "source branch updates"

  write_scenario_metadata merge-back
}

json_escape() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\n'/\\n}"
  printf '%s' "$value"
}

json_array_from_lines() {
  local lines="$1"
  local first=1
  printf '['
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    if (( first )); then
      first=0
    else
      printf ', '
    fi
    printf '"%s"' "$(json_escape "$line")"
  done <<< "$lines"
  printf ']'
}

write_scenario_metadata() {
  local scenario="$1"
  local root
  local action
  local style
  local open_paths
  local conflicts
  local staged
  local notes

  root="$(scenario_dir "$scenario")"
  action="$(scenario_entry_action "$scenario")"
  style="$(scenario_conflict_style "$scenario")"
  open_paths="$(scenario_open_paths "$scenario")"
  conflicts="$(scenario_expected_conflicts "$scenario")"
  staged="$(scenario_expected_staged "$scenario")"
  notes="$(scenario_notes "$scenario")"

  write_file "$root/SCENARIO.md" <<EOF
# $(scenario_title "$scenario")

- Scenario ID: \`$scenario\`
- Scenario root: \`$root\`
- Entry action: $(entry_action_text "$action")
- Conflict markers: \`$style\`

## Open Paths
$(render_markdown_list "$open_paths")

## Expected Conflicted Files
$(render_markdown_list "$conflicts")

## Expected Staged Files After Entry
$(render_markdown_list "$staged")

## Notes
$(render_markdown_list "$notes")
EOF

  write_file "$root/scenario.json" <<EOF
{
  "id": "$(json_escape "$scenario")",
  "display_name": "$(json_escape "$(scenario_title "$scenario")")",
  "open_paths": $(json_array_from_lines "$open_paths"),
  "entry_action": "$(json_escape "$action")",
  "expected_conflicted_files": $(json_array_from_lines "$conflicts"),
  "expected_staged_files": $(json_array_from_lines "$staged"),
  "conflict_style": "$(json_escape "$style")",
  "notes": $(json_array_from_lines "$notes")
}
EOF
}

render_markdown_list() {
  local lines="$1"
  local output=""
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    output+="- $line"$'\n'
  done <<< "$lines"
  printf '%s' "$output"
}

print_setup_summary() {
  local scenario="$1"
  printf 'Created %s\n' "$(scenario_dir "$scenario")"
  describe_scenario "$scenario"
  printf 'Metadata: %s/SCENARIO.md\n' "$(scenario_dir "$scenario")"
}

print_all_summary() {
  local scenario
  printf 'Fixtures created under:\n  %s\n\n' "$BASE_DIR"
  for scenario in "${SCENARIOS[@]}"; do
    printf '%s\n' "$scenario"
    printf '  %s\n' "$(scenario_summary "$scenario")"
    printf '  metadata: %s/SCENARIO.md\n' "$(scenario_dir "$scenario")"
    while IFS= read -r path; do
      [[ -z "$path" ]] && continue
      printf '  open: %s\n' "$path"
    done <<< "$(scenario_open_paths "$scenario")"
    printf '\n'
  done
}

main "$@"
