# OrcaShell shell integration for Bash
# Emits OSC 133 semantic prompt markers and OSC 2 titles.

# Guard against double-sourcing.
[[ -n "$__orcashell_integrated" ]] && return
__orcashell_integrated=1

__orcashell_command_running=""
__orcashell_in_debug_trap=""
__orcashell_in_prompt_command=""
__orcashell_prev_prompt_command="${PROMPT_COMMAND:-}"
__orcashell_prev_debug_trap="$(trap -p DEBUG)"

if [[ "$__orcashell_prev_debug_trap" == "trap -- '"*"' DEBUG" ]]; then
    __orcashell_prev_debug_trap=${__orcashell_prev_debug_trap#"trap -- '"}
    __orcashell_prev_debug_trap=${__orcashell_prev_debug_trap%"' DEBUG"}
else
    __orcashell_prev_debug_trap=""
fi

__orcashell_sanitize_title() {
    local title="$1"
    title=${title//$'\a'/ }
    title=${title//$'\e'/ }
    title=${title//$'\r'/ }
    title=${title//$'\n'/ }
    printf '%s' "$title"
}

__orcashell_set_title() {
    printf '\e]2;%s\a' "$(__orcashell_sanitize_title "$1")"
}

__orcashell_prompt_title() {
    local title="${PWD##*/}"
    [[ -z "$title" ]] && title="$PWD"
    [[ -z "$title" ]] && title="bash"
    __orcashell_set_title "$title"
}

__orcashell_prompt_command() {
    local exit_status=$?
    __orcashell_in_prompt_command=1
    if [[ -n "$__orcashell_prev_prompt_command" ]]; then
        eval "$__orcashell_prev_prompt_command"
    fi
    __orcashell_in_prompt_command=""
    # D marker for previous command (skip on first prompt).
    if [[ -n "$__orcashell_command_running" ]]; then
        printf '\e]133;D;%s\a' "$exit_status"
        __orcashell_command_running=""
    fi
    # A marker: prompt starts.
    printf '\e]133;A\a'
    __orcashell_prompt_title
}

__orcashell_preexec() {
    local current_command="${1:-$BASH_COMMAND}"
    # Skip during tab completion.
    [[ -n "$COMP_LINE" ]] && return
    # Skip our own prompt command.
    [[ "$current_command" == "__orcashell_prompt_command" ]] && return
    [[ "$current_command" == "__orcashell_debug_trap" ]] && return
    # Skip if already marked (trap fires per simple command).
    [[ -n "$__orcashell_command_running" ]] && return
    __orcashell_command_running=1
    # B marker: user input complete, command starts.
    printf '\e]133;B\a'
    # C marker: command is now executing.
    printf '\e]133;C\a'
    __orcashell_set_title "$current_command"
}

__orcashell_debug_trap() {
    local current_command="$BASH_COMMAND"
    [[ -n "$__orcashell_in_prompt_command" ]] && return
    [[ -n "$__orcashell_in_debug_trap" ]] && return

    __orcashell_in_debug_trap=1
    if [[ -n "$__orcashell_prev_debug_trap" ]]; then
        eval "$__orcashell_prev_debug_trap"
    fi
    __orcashell_preexec "$current_command"
    __orcashell_in_debug_trap=""
}

# Match OrcaShell's Alt+Arrow xterm-style escape sequences to readline word movement.
bind '"\e[1;3D": backward-word'
bind '"\e[1;3C": forward-word'

PROMPT_COMMAND="__orcashell_prompt_command"
trap '__orcashell_debug_trap' DEBUG
