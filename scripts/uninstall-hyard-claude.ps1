$destSkills = Join-Path $env:USERPROFILE ".claude\skills"
$destSkill = Join-Path $destSkills "hyard.md"
$destManifest = Join-Path $env:USERPROFILE ".claude\hyard-native-manifest.yaml"

if (Test-Path $destSkill) {
    Remove-Item $destSkill -Force
    Write-Output "Removed Claude HYARD skill."
} else {
    Write-Output "Claude HYARD skill not found, skipping."
}

if (Test-Path $destManifest) {
    Remove-Item $destManifest -Force
    Write-Output "Removed Claude HYARD manifest."
} else {
    Write-Output "Claude HYARD manifest not found, skipping."
}
