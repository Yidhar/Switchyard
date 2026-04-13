$destSkills = Join-Path $env:USERPROFILE ".gemini\skills"
$destSkill = Join-Path $destSkills "hyard.md"
$destExtension = Join-Path $env:USERPROFILE ".gemini\hyard-extension.yaml"

if (Test-Path $destSkill) {
    Remove-Item $destSkill -Force
    Write-Output "Removed Gemini HYARD skill."
} else {
    Write-Output "Gemini HYARD skill not found, skipping."
}

if (Test-Path $destExtension) {
    Remove-Item $destExtension -Force
    Write-Output "Removed Gemini HYARD extension manifest."
} else {
    Write-Output "Gemini HYARD extension manifest not found, skipping."
}
