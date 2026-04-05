# OrcaShell shell integration for Zsh
# Emits OSC 133 semantic prompt markers and OSC 2 titles.

# Guard against double-sourcing.
[[ -n "$__orcashell_integrated" ]] && return
__orcashell_integrated=1

__orcashell_command_running=""

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
    local title="${PWD:t}"
    [[ -z "$title" ]] && title="$PWD"
    [[ -z "$title" ]] && title="zsh"
    __orcashell_set_title "$title"
}

__orcashell_precmd() {
    local exit_status=$?
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
    local command="$1"
    __orcashell_command_running=1
    # B marker: user input complete, command starts.
    printf '\e]133;B\a'
    # C marker: command is now executing.
    printf '\e]133;C\a'
    __orcashell_set_title "$command"
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd __orcashell_precmd
add-zsh-hook preexec __orcashell_preexec
