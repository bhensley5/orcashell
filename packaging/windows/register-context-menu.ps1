# Register "Open OrcaShell Here" in Windows Explorer context menu.
# Uses HKCU - no elevation required.
# Shows under "Show more options" in Windows 11, directly in Windows 10.
#
# Usage: .\register-context-menu.ps1 [-OrcashPath <path>] [-OrcashellPath <path>]
#   Defaults to orcash.exe and orcashell.exe next to this script.

param(
    [string]$OrcashPath,
    [string]$OrcashellPath
)

$ErrorActionPreference = "Stop"

# Auto-discover binaries next to this script if not provided
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

if (-not $OrcashPath) {
    $OrcashPath = Join-Path $ScriptDir "orcash.exe"
}
if (-not $OrcashellPath) {
    $OrcashellPath = Join-Path $ScriptDir "orcashell.exe"
}

if (-not (Test-Path $OrcashPath)) {
    Write-Error "orcash.exe not found at $OrcashPath"
    exit 1
}

# ── Right-click on a folder ──
$folderKey = "HKCU:\SOFTWARE\Classes\Directory\shell\OrcaShell"
New-Item -Path $folderKey -Force | Out-Null
Set-ItemProperty -Path $folderKey -Name "(Default)" -Value "Open OrcaShell Here"
if (Test-Path $OrcashellPath) {
    Set-ItemProperty -Path $folderKey -Name "Icon" -Value "`"$OrcashellPath`",0"
}

$folderCmd = Join-Path $folderKey "command"
New-Item -Path $folderCmd -Force | Out-Null
Set-ItemProperty -Path $folderCmd -Name "(Default)" -Value "`"$OrcashPath`" open --dir `"%V`""

# ── Right-click on folder background (inside a folder) ──
$bgKey = "HKCU:\SOFTWARE\Classes\Directory\Background\shell\OrcaShell"
New-Item -Path $bgKey -Force | Out-Null
Set-ItemProperty -Path $bgKey -Name "(Default)" -Value "Open OrcaShell Here"
if (Test-Path $OrcashellPath) {
    Set-ItemProperty -Path $bgKey -Name "Icon" -Value "`"$OrcashellPath`",0"
}

$bgCmd = Join-Path $bgKey "command"
New-Item -Path $bgCmd -Force | Out-Null
Set-ItemProperty -Path $bgCmd -Name "(Default)" -Value "`"$OrcashPath`" open --dir `"%V`""

Write-Host "Registered 'Open OrcaShell Here' context menu entries (HKCU)."
Write-Host "  Folder:     $folderKey"
Write-Host "  Background: $bgKey"
Write-Host ""
Write-Host "On Windows 11, the entry appears under 'Show more options'."
