$destCodex = Join-Path $env:USERPROFILE ".codex"
$destAgents = Join-Path $destCodex "AGENTS.md"
$destSkillDir = Join-Path $destCodex "skills\hyard"
$legacyFlatSkill = Join-Path $destCodex "skills\hyard.md"

if (Test-Path $destSkillDir) {
    Remove-Item $destSkillDir -Recurse -Force
    Write-Output "Removed Codex HYARD skill directory."
} else {
    Write-Output "Codex HYARD skill directory not found, skipping."
}

if (Test-Path $legacyFlatSkill) {
    Remove-Item $legacyFlatSkill -Force
    Write-Output "Removed legacy Codex HYARD flat skill file."
}

if (Test-Path $destAgents) {
    Remove-Item $destAgents -Force
    Write-Output "Removed Codex AGENTS instructions."
} else {
    Write-Output "Codex AGENTS instructions not found, skipping."
}
