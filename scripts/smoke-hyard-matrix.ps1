param(
    [string[]]$Providers = @("codex", "claude", "gemini"),
    [string]$TaskTemplate = "Say exactly: hyard smoke {provider}",
    [ValidateRange(0, 3600)]
    [int]$InitialWaitSec = 5,
    [ValidateRange(0, 3600)]
    [int]$AwaitTimeoutSec = 30,
    [string]$ReportDir,
    [switch]$SkipCancel,
    [switch]$RequireTerminal,
    [switch]$ContinueOnError
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$singleScript = Join-Path $PSScriptRoot "smoke-hyard-host.ps1"
if (-not (Test-Path $singleScript)) {
    throw "Missing smoke script: $singleScript"
}

if ($Providers.Count -eq 1 -and $Providers[0] -match ",") {
    $Providers = @(
        $Providers[0].Split(",") |
            ForEach-Object { $_.Trim() } |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    )
}

if (-not $ReportDir) {
    $ReportDir = Join-Path $repoRoot (".switchyard\smoke\hyard\matrix-" + (Get-Date -Format "yyyyMMdd-HHmmss"))
}

if (-not (Test-Path $ReportDir)) {
    New-Item -ItemType Directory -Path $ReportDir -Force | Out-Null
}

$shellExe = (Get-Process -Id $PID).Path
$matrix = [ordered]@{
    smoke_protocol   = "hyard_smoke_matrix_v1"
    started_at       = (Get-Date).ToString("o")
    report_dir       = $ReportDir
    providers        = @()
}

Write-Host "Starting HYARD smoke matrix"
Write-Host "  report_dir: $ReportDir"
Write-Host "  shell:      $shellExe"

foreach ($provider in $Providers) {
    $task = $TaskTemplate.Replace("{provider}", $provider)
    $reportPath = Join-Path $ReportDir "$provider.json"

    $args = @(
        "-ExecutionPolicy", "Bypass",
        "-File", $singleScript,
        "-Provider", $provider,
        "-Task", $task,
        "-InitialWaitSec", $InitialWaitSec,
        "-AwaitTimeoutSec", $AwaitTimeoutSec,
        "-ReportPath", $reportPath
    )
    if ($SkipCancel) {
        $args += "-SkipCancel"
    }
    if ($RequireTerminal) {
        $args += "-RequireTerminal"
    }

    Write-Host ""
    Write-Host "## Provider: $provider"
    Write-Host ("> " + $shellExe + " " + ($args -join " "))

    & $shellExe @args
    $exitCode = $LASTEXITCODE

    $reportJson = $null
    if (Test-Path $reportPath) {
        try {
            if ((Get-Command ConvertFrom-Json).Parameters.ContainsKey("Depth")) {
                $reportJson = Get-Content $reportPath -Raw | ConvertFrom-Json -Depth 64
            }
            else {
                $reportJson = Get-Content $reportPath -Raw | ConvertFrom-Json
            }
        }
        catch {
            $reportJson = [pscustomobject]@{
                summary = [pscustomobject]@{
                    passed = $false
                    error  = "failed to parse report json: $($_.Exception.Message)"
                }
            }
        }
    }

    $matrix.providers += [pscustomobject]@{
        provider    = $provider
        exit_code   = $exitCode
        report_path = $reportPath
        summary     = if ($reportJson) { $reportJson.summary } else { $null }
    }

    if ($exitCode -ne 0 -and -not $ContinueOnError) {
        break
    }
}

$matrix.finished_at = (Get-Date).ToString("o")
$matrix.summary = [ordered]@{
    total   = $matrix.providers.Count
    passed  = @($matrix.providers | Where-Object { $_.exit_code -eq 0 }).Count
    failed  = @($matrix.providers | Where-Object { $_.exit_code -ne 0 }).Count
}

$matrixPath = Join-Path $ReportDir "matrix.json"
$matrix | ConvertTo-Json -Depth 64 | Set-Content -Path $matrixPath -Encoding utf8

Write-Host ""
Write-Host "HYARD smoke matrix summary"
$matrix.providers |
    Select-Object provider, exit_code,
        @{ Name = "passed"; Expression = { if ($_.summary) { $_.summary.passed } else { $false } } },
        @{ Name = "delegate_status"; Expression = { if ($_.summary) { $_.summary.delegate_status } else { $null } } },
        @{ Name = "final_status"; Expression = { if ($_.summary) { $_.summary.final_status } else { $null } } },
        report_path |
    Format-Table -AutoSize

Write-Host ""
Write-Host "Matrix report: $matrixPath"

if ($matrix.summary.failed -gt 0) {
    exit 1
}
