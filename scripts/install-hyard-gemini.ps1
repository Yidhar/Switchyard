param(
    [switch]$Force
)

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$repoRoot = Resolve-Path "$scriptDir\.."
$geminiSkillSource = Join-Path $repoRoot "host-packs\gemini\hyard-skill.md"
$extensionSource = Join-Path $repoRoot "host-packs\gemini\extension\manifest.yaml"
$destSkills = Join-Path $env:USERPROFILE ".gemini\skills"
$destSkill = Join-Path $destSkills "hyard.md"
$destExtension = Join-Path $env:USERPROFILE ".gemini\hyard-extension.yaml"
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

if (-not (Test-Path $geminiSkillSource)) {
    Write-Error "Missing $geminiSkillSource"
    exit 1
}

$switchyardCmd = Resolve-SwitchyardCommand

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
Write-Output "Now use `gemini extensions link <path>` if building an extension."
