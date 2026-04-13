param(
    [switch]$Force
)

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$repoRoot = Resolve-Path "$scriptDir\.."
$agentsSource = Join-Path $repoRoot "host-packs\codex\AGENTS.md"
$skillSource = Join-Path $repoRoot "host-packs\codex\skills\hyard\SKILL.md"
$destCodex = Join-Path $env:USERPROFILE ".codex"
$destAgents = Join-Path $destCodex "AGENTS.md"
$destSkillsDir = Join-Path $destCodex "skills"
$destSkillDir = Join-Path $destSkillsDir "hyard"
$destSkill = Join-Path $destSkillDir "SKILL.md"
$legacyFlatSkill = Join-Path $destSkillsDir "hyard.md"
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)

function Resolve-SwitchyardCommand {
    $command = Get-Command switchyard -ErrorAction SilentlyContinue
    if ($command) {
        return "switchyard"
    }

    $candidates = @(
        (Join-Path $repoRoot "target\debug\switchyard.exe"),
        (Join-Path $repoRoot "target\release\switchyard.exe")
    )

    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) {
            if ($candidate -match '\s') {
                return '"' + $candidate + '"'
            }
            return $candidate
        }
    }

    Write-Warning "switchyard binary not found on PATH or under target\\{debug,release}; installed instructions will keep the generic 'switchyard' command."
    return "switchyard"
}

function Write-Utf8NoBom([string]$Path, [string]$Content) {
    [System.IO.File]::WriteAllText($Path, $Content, $Utf8NoBom)
}

if (-not (Test-Path $agentsSource)) {
    Write-Error "Missing $agentsSource"
    exit 1
}

if (-not (Test-Path $skillSource)) {
    Write-Error "Missing $skillSource"
    exit 1
}

$switchyardCmd = Resolve-SwitchyardCommand

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
