param(
    [switch]$Force
)

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$repoRoot = Resolve-Path "$scriptDir\.."
$claudeSkillSource = Join-Path $repoRoot "host-packs\claude\hyard-skill.md"
$manifestSource = Join-Path $repoRoot "host-packs\claude\native\manifest.yaml"
$destSkills = Join-Path $env:USERPROFILE ".claude\skills"
$destSkill = Join-Path $destSkills "hyard.md"
$destManifest = Join-Path $env:USERPROFILE ".claude\hyard-native-manifest.yaml"
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

    Write-Warning "switchyard binary not found on PATH or under target\\{debug,release}; installed skill will keep the generic 'switchyard' command."
    return "switchyard"
}

function Write-Utf8NoBom([string]$Path, [string]$Content) {
    [System.IO.File]::WriteAllText($Path, $Content, $Utf8NoBom)
}

if (-not (Test-Path $claudeSkillSource)) {
    Write-Error "Missing $claudeSkillSource"
    exit 1
}

$switchyardCmd = Resolve-SwitchyardCommand

if (-not (Test-Path $destSkills)) {
    Write-Output "Creating Claude skill directory: $destSkills"
    New-Item -ItemType Directory -Path $destSkills -Force | Out-Null
}

$skillContent = (Get-Content $claudeSkillSource -Raw).Replace("{{SWITCHYARD_CMD}}", $switchyardCmd)
$manifestContent = (Get-Content $manifestSource -Raw).Replace("{{SWITCHYARD_CMD}}", $switchyardCmd)

Write-Utf8NoBom -Path $destSkill -Content $skillContent
Write-Utf8NoBom -Path $destManifest -Content $manifestContent

Write-Output "Claude HYARD skill installed:"
Write-Output "  Skill: $destSkill"
Write-Output "  Manifest: $destManifest"
Write-Output "  Switchyard command: $switchyardCmd"
Write-Output "Run Claude and ensure the skill is loaded, then use `/hyard:list` to verify."
