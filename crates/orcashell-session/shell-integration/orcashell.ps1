# OrcaShell PowerShell integration. OSC 133 semantic prompts and OSC 2 title.
#
# Uses [char]0x1b and [char]0x07 for Windows PowerShell 5.1 compatibility.
# Do NOT replace with `e or `a escapes - those require PS 7+.

if ($env:__orcashell_integrated) { return }
$env:__orcashell_integrated = "1"

$__orcashell_esc = [char]0x1b
$__orcashell_bel = [char]0x07

# Save original prompt function
$__orcashell_original_prompt = if (Test-Path Function:\prompt) {
    (Get-Item Function:\prompt).ScriptBlock
} else {
    { "PS $($executionContext.SessionState.Path.CurrentLocation)$('>' * ($nestedPromptLevel + 1)) " }
}

$__orcashell_first_prompt = $true

function prompt {
    # Capture exit state before anything else.
    # Do NOT clear $LASTEXITCODE. Users expect it to persist until the next
    # external command, and clearing it here would be a shell regression.
    $__exit = if ($global:LASTEXITCODE) { $global:LASTEXITCODE } else { 0 }
    if (-not $?) { $__exit = 1 }

    # D marker for previous command (skip on first prompt)
    if (-not $script:__orcashell_first_prompt) {
        [Console]::Write("${__orcashell_esc}]133;D;${__exit}${__orcashell_bel}")
    }
    $script:__orcashell_first_prompt = $false

    # A marker: prompt starts
    [Console]::Write("${__orcashell_esc}]133;A${__orcashell_bel}")

    # Title: current directory basename (OSC 2)
    $__dir = Split-Path -Leaf (Get-Location)
    if (-not $__dir) { $__dir = (Get-Location).Path }
    if (-not $__dir) { $__dir = "pwsh" }
    # Sanitize title: remove control characters
    $__dir = $__dir -replace '[\x00-\x1f\x07]', ''
    [Console]::Write("${__orcashell_esc}]2;${__dir}${__orcashell_bel}")

    # Run original prompt to get prompt string
    $__prompt_text = & $__orcashell_original_prompt

    # B marker: end of prompt, user input area starts
    $__result = "${__prompt_text}${__orcashell_esc}]133;B${__orcashell_bel}"
    return $__result
}

# C marker: command is about to execute (via PSReadLine Enter key wrapper).
# Explicitly import PSReadLine. It may be available but not yet loaded when
# this script runs (e.g., via -File before profile processing completes).
Import-Module PSReadLine -ErrorAction SilentlyContinue
if (Get-Module PSReadLine) {
    # Preserve the user's existing Enter binding so we wrap rather than replace.
    # This avoids breaking profile-driven key behavior or plugin integrations.
    $__orcashell_prev_enter_handler = $null
    $__orcashell_prev_enter_info = Get-PSReadLineKeyHandler -Bound |
        Where-Object { $_.Key -eq 'Enter' } |
        Select-Object -First 1
    if ($__orcashell_prev_enter_info) {
        $__orcashell_prev_enter_handler = $__orcashell_prev_enter_info.Function
    }

    Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
        [Console]::Write("${__orcashell_esc}]133;C${__orcashell_bel}")
        # Invoke the previous binding (or AcceptLine as default)
        $handler = $__orcashell_prev_enter_handler
        if ($handler -and $handler -ne 'AcceptLine') {
            # Call the previously-bound PSReadLine function by name
            $method = [Microsoft.PowerShell.PSConsoleReadLine].GetMethod($handler,
                [System.Reflection.BindingFlags]::Public -bor [System.Reflection.BindingFlags]::Static,
                $null, [Type]::EmptyTypes, $null)
            if ($method) {
                $method.Invoke($null, @())
            } else {
                # Fallback if reflection fails
                [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
            }
        } else {
            [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
        }
    }
}
