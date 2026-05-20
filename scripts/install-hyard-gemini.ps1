param(
    [switch]$Force
)

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$repoRoot = Resolve-Path "$scriptDir\.."
. (Join-Path $scriptDir "lib-hyard-install.ps1")
$geminiSkillSource = Join-Path $repoRoot "host-packs\gemini\hyard-skill.md"
$extensionSource = Join-Path $repoRoot "host-packs\gemini\extension\manifest.yaml"
$destSkills = Join-Path $env:USERPROFILE ".gemini\skills"
$destSkill = Join-Path $destSkills "hyard.md"
$destExtension = Join-Path $env:USERPROFILE ".gemini\hyard-extension.yaml"

if (-not (Test-Path $geminiSkillSource)) {
    Write-Error "Missing $geminiSkillSource"
    exit 1
}

$switchyard = Resolve-SwitchyardCommand $repoRoot
$switchyardCmd = $switchyard.Command

if (-not (Test-Path $destSkills)) {
    Write-Output "Creating Gemini skill directory: $destSkills"
    New-Item -ItemType Directory -Path $destSkills -Force | Out-Null
}

$skillContent = (Get-Content $geminiSkillSource -Raw).Replace("{{SWITCHYARD_CMD}}", $switchyardCmd)
$extensionContent = (Get-Content $extensionSource -Raw).Replace("{{SWITCHYARD_CMD}}", $switchyardCmd)

Write-Utf8NoBom -Path $destSkill -Content $skillContent
Write-Utf8NoBom -Path $destExtension -Content $extensionContent

Write-Output "Gemini HYARD skill + extension manifest installed."
Write-Output "  Skill: $destSkill"
Write-Output "  Extension manifest: $destExtension"
Write-Output "  Switchyard command: $switchyardCmd"
if ($switchyard.Source -eq "shim") {
    Write-Output "  Installed short-command shim: $($switchyard.Path)"
}
Write-Output "Now use `gemini extensions link <path>` if building an extension."
