param(
    [switch]$Force
)

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$repoRoot = Resolve-Path "$scriptDir\.."
. (Join-Path $scriptDir "lib-hyard-install.ps1")
$agentsSource = Join-Path $repoRoot "host-packs\codex\AGENTS.md"
$skillSource = Join-Path $repoRoot "host-packs\codex\skills\hyard\SKILL.md"
$destCodex = Join-Path $env:USERPROFILE ".codex"
$destAgents = Join-Path $destCodex "AGENTS.md"
$destSkillsDir = Join-Path $destCodex "skills"
$destSkillDir = Join-Path $destSkillsDir "hyard"
$destSkill = Join-Path $destSkillDir "SKILL.md"
$legacyFlatSkill = Join-Path $destSkillsDir "hyard.md"

if (-not (Test-Path $agentsSource)) {
    Write-Error "Missing $agentsSource"
    exit 1
}

if (-not (Test-Path $skillSource)) {
    Write-Error "Missing $skillSource"
    exit 1
}

$switchyard = Resolve-SwitchyardCommand $repoRoot
$switchyardCmd = $switchyard.Command

if (-not (Test-Path $destCodex)) {
    Write-Output "Creating Codex config directory: $destCodex"
    New-Item -ItemType Directory -Path $destCodex -Force | Out-Null
}

if (-not (Test-Path $destSkillDir)) {
    New-Item -ItemType Directory -Path $destSkillDir -Force | Out-Null
}

if (Test-Path $legacyFlatSkill) {
    Remove-Item $legacyFlatSkill -Force
}

$agentsContent = (Get-Content $agentsSource -Raw).Replace("{{SWITCHYARD_CMD}}", $switchyardCmd)
$skillContent = (Get-Content $skillSource -Raw).Replace("{{SWITCHYARD_CMD}}", $switchyardCmd)

Write-Utf8NoBom -Path $destAgents -Content $agentsContent
Write-Utf8NoBom -Path $destSkill -Content $skillContent

Write-Output "Codex HYARD instructions installed."
Write-Output "  AGENTS: $destAgents"
Write-Output "  Skill: $destSkill"
Write-Output "  Switchyard command: $switchyardCmd"
if ($switchyard.Source -eq "shim") {
    Write-Output "  Installed short-command shim: $($switchyard.Path)"
}
