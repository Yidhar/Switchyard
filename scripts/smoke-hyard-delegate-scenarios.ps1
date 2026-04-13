param(
    [string]$CoreProvider = "codex",
    [string[]]$PeerProviders = @("claude", "gemini"),
    [string]$WaitTimeoutProvider = "claude",
    [ValidateRange(0, 3600)]
    [int]$InitialWaitSec = 0,
    [ValidateRange(1, 3600)]
    [int]$AwaitTimeoutSec = 60,
    [string]$ReportDir,
    [switch]$SkipCheck,
    [switch]$ContinueOnError
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$routedScript = Join-Path $PSScriptRoot "smoke-routed-delegate.ps1"
$hostScript = Join-Path $PSScriptRoot "smoke-hyard-host.ps1"

if (-not (Test-Path $routedScript)) {
    throw "Missing routed delegate smoke script: $routedScript"
}
if (-not (Test-Path $hostScript)) {
    throw "Missing host smoke script: $hostScript"
}

if (-not $ReportDir) {
    $ReportDir = Join-Path $repoRoot (".switchyard\smoke\delegate-scenarios-" + (Get-Date -Format "yyyyMMdd-HHmmss"))
}

if (-not (Test-Path $ReportDir)) {
    New-Item -ItemType Directory -Path $ReportDir -Force | Out-Null
}

$shellExe = (Get-Process -Id $PID).Path
$matrix = [ordered]@{
    smoke_protocol = "hyard_delegate_scenarios_v1"
    started_at     = (Get-Date).ToString("o")
    report_dir     = $ReportDir
    core_provider  = $CoreProvider
    peer_providers = @($PeerProviders)
    scenarios      = @()
}

function Read-ReportJson {
    param([string]$Path)

    if (-not (Test-Path $Path)) {
        return $null
    }

    $raw = Get-Content $Path -Raw
    if ([string]::IsNullOrWhiteSpace($raw)) {
        return $null
    }

    if ((Get-Command ConvertFrom-Json).Parameters.ContainsKey("Depth")) {
        return $raw | ConvertFrom-Json -Depth 64
    }

    return $raw | ConvertFrom-Json
}

function Add-ScenarioResult {
    param(
        [string]$Name,
        [int]$ExitCode,
        [string]$ReportPath
    )

    $report = Read-ReportJson -Path $ReportPath
    $matrix.scenarios += [pscustomobject]@{
        name       = $Name
        exit_code  = $ExitCode
        report_path = $ReportPath
        summary    = if ($report) { $report.summary } else { $null }
    }
}

Write-Host "Starting HYARD delegate scenario smoke"
Write-Host "  report_dir: $ReportDir"
Write-Host "  shell:      $shellExe"

foreach ($peer in $PeerProviders) {
    $reportPath = Join-Path $ReportDir ("routed-" + $CoreProvider + "-to-" + $peer + ".json")
    $args = @(
        "-ExecutionPolicy", "Bypass",
        "-File", $routedScript,
        "-CoreProvider", $CoreProvider,
        "-PeerProvider", $peer,
        "-ReportPath", $reportPath
    )
    if ($SkipCheck) {
        $args += "-SkipCheck"
    }

    Write-Host ""
    Write-Host "## Routed scenario: $CoreProvider -> $peer"
    Write-Host ("> " + $shellExe + " " + ($args -join " "))

    & $shellExe @args
    $exitCode = $LASTEXITCODE
    Add-ScenarioResult -Name ("routed:" + $CoreProvider + "->" + $peer) -ExitCode $exitCode -ReportPath $reportPath

    if ($exitCode -ne 0 -and -not $ContinueOnError) {
        break
    }
}

if ($matrix.scenarios.Count -eq $PeerProviders.Count -or $ContinueOnError) {
    $reportPath = Join-Path $ReportDir ("host-wait-timeout-" + $WaitTimeoutProvider + ".json")
    $args = @(
        "-ExecutionPolicy", "Bypass",
        "-File", $hostScript,
        "-Provider", $WaitTimeoutProvider,
        "-Task", "Reply with exactly: wait-timeout-ok",
        "-InitialWaitSec", $InitialWaitSec,
        "-AwaitTimeoutSec", $AwaitTimeoutSec,
        "-SkipCancel",
        "-RequireWaitTimeout",
        "-RequireAwait",
        "-RequireTerminal",
        "-ReportPath", $reportPath
    )

    Write-Host ""
    Write-Host "## Async bridge scenario: wait_timeout -> status/result/await ($WaitTimeoutProvider)"
    Write-Host ("> " + $shellExe + " " + ($args -join " "))

    & $shellExe @args
    $exitCode = $LASTEXITCODE
    Add-ScenarioResult -Name ("host-wait-timeout:" + $WaitTimeoutProvider) -ExitCode $exitCode -ReportPath $reportPath
}

$matrix.finished_at = (Get-Date).ToString("o")
$matrix.summary = [ordered]@{
    total  = $matrix.scenarios.Count
    passed = @($matrix.scenarios | Where-Object { $_.exit_code -eq 0 }).Count
    failed = @($matrix.scenarios | Where-Object { $_.exit_code -ne 0 }).Count
}

$matrixPath = Join-Path $ReportDir "matrix.json"
$matrix | ConvertTo-Json -Depth 64 | Set-Content -Path $matrixPath -Encoding utf8

Write-Host ""
Write-Host "HYARD delegate scenario summary"
$matrix.scenarios |
    Select-Object name, exit_code,
        @{ Name = "passed"; Expression = { if ($_.summary) { $_.summary.passed } else { $false } } },
        @{ Name = "final_status"; Expression = {
            if (-not $_.summary) {
                return $null
            }
            if ($_.summary.PSObject.Properties["final_status"]) {
                return $_.summary.final_status
            }
            if ($_.summary.PSObject.Properties["final_response"]) {
                return $_.summary.final_response
            }
            return $null
        } },
        report_path |
    Format-Table -AutoSize

Write-Host ""
Write-Host "Matrix report: $matrixPath"

if ($matrix.summary.failed -gt 0) {
    exit 1
}
