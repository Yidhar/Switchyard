param(
    [switch]$Force
)

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$repoRoot = Resolve-Path "$scriptDir\.."
. (Join-Path $scriptDir "lib-hyard-install.ps1")
$claudeSkillSource = Join-Path $repoRoot "host-packs\claude\hyard-skill.md"
$manifestSource = Join-Path $repoRoot "host-packs\claude\native\manifest.yaml"
$destSkills = Join-Path $env:USERPROFILE ".claude\skills"
$destSkill = Join-Path $destSkills "hyard.md"
$destManifest = Join-Path $env:USERPROFILE ".claude\hyard-native-manifest.yaml"

if (-not (Test-Path $claudeSkillSource)) {
    Write-Error "Missing $claudeSkillSource"
    exit 1
}

$switchyard = Resolve-SwitchyardCommand $repoRoot
$switchyardCmd = $switchyard.Command

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
if ($switchyard.Source -eq "shim") {
    Write-Output "  Installed short-command shim: $($switchyard.Path)"
}
Write-Output "Run Claude and ensure the skill is loaded, then use `/hyard:list` to verify."
