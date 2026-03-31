# Remove "Open OrcaShell Here" from Windows Explorer context menu.
# Reverses what register-context-menu.ps1 creates.

$ErrorActionPreference = "SilentlyContinue"

$folderKey = "HKCU:\SOFTWARE\Classes\Directory\shell\OrcaShell"
$bgKey = "HKCU:\SOFTWARE\Classes\Directory\Background\shell\OrcaShell"

$removed = 0

if (Test-Path $folderKey) {
    Remove-Item -Path $folderKey -Recurse -Force
    $removed++
}

if (Test-Path $bgKey) {
    Remove-Item -Path $bgKey -Recurse -Force
    $removed++
}

if ($removed -gt 0) {
    Write-Host "Removed OrcaShell context menu entries."
} else {
    Write-Host "No OrcaShell context menu entries found."
}
