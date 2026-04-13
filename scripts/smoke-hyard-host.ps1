param(
    [string]$Provider = "codex",
    [string]$Task = "Say exactly: smoke hyard",
    [ValidateRange(0, 3600)]
    [int]$InitialWaitSec = 5,
    [ValidateRange(0, 3600)]
    [int]$AwaitTimeoutSec = 30,
    [string]$SwitchyardExePath,
    [string]$ReportPath,
    [switch]$SkipCancel,
    [switch]$RequireTerminal,
    [switch]$RequireWaitTimeout,
    [switch]$RequireAwait
)

$ErrorActionPreference = "Stop"

$script:RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$script:Report = [ordered]@{
    smoke_protocol   = "hyard_smoke_v1"
    started_at       = (Get-Date).ToString("o")
    provider         = $Provider
    task             = $Task
    initial_wait_sec = $InitialWaitSec
    await_timeout_sec = $AwaitTimeoutSec
    steps            = @()
    summary          = [ordered]@{}
}

function Resolve-SwitchyardExe {
    param([string]$PreferredPath)

    $candidates = @()
    if ($PreferredPath) {
        $candidates += $PreferredPath
    }

    $candidates += @(
        (Join-Path $script:RepoRoot "target\debug\switchyard.exe"),
        (Join-Path $script:RepoRoot "target\release\switchyard.exe")
    )

    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path $candidate)) {
            return (Resolve-Path $candidate).Path
        }
    }

    $command = Get-Command switchyard -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    throw "switchyard binary not found. Build first with cargo build or pass -SwitchyardExePath."
}

function Ensure-Directory {
    param([string]$Path)
    $dir = Split-Path -Parent $Path
    if ($dir -and -not (Test-Path $dir)) {
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
    }
}

function Default-ReportPath {
    $timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $reportDir = Join-Path $script:RepoRoot ".switchyard\smoke\hyard"
    if (-not (Test-Path $reportDir)) {
        New-Item -ItemType Directory -Path $reportDir -Force | Out-Null
    }
    return Join-Path $reportDir ("{0}-{1}.json" -f $timestamp, $Provider)
}

function Assert-Condition {
    param(
        [bool]$Condition,
        [string]$Message
    )

    if (-not $Condition) {
        throw $Message
    }
}

function Convert-BridgeOutput {
    param(
        [string]$Raw,
        [string]$Label
    )

    Assert-Condition (-not [string]::IsNullOrWhiteSpace($Raw)) "$Label produced empty stdout."

    try {
        if ((Get-Command ConvertFrom-Json).Parameters.ContainsKey("Depth")) {
            return $Raw | ConvertFrom-Json -Depth 64
        }
        return $Raw | ConvertFrom-Json
    }
    catch {
        throw "$Label produced non-JSON stdout.`nRaw:`n$Raw"
    }
}

function Get-JsonField {
    param(
        $Json,
        [string]$Name
    )

    if ($null -eq $Json) {
        return $null
    }

    $property = $Json.PSObject.Properties[$Name]
    if ($null -eq $property) {
        return $null
    }

    return $property.Value
}

function Is-ActiveStatus {
    param([string]$Status)
    return @("queued", "running", "cancel_requested", "pending", "wait_timeout") -contains $Status
}

function Is-TerminalStatus {
    param([string]$Status)
    return @("completed", "failed", "cancelled") -contains $Status
}

function Assert-BridgeEnvelope {
    param(
        $Json,
        [string]$ExpectedCommand,
        [switch]$AllowMissingCommand
    )

    Assert-Condition ($null -ne $Json) "Bridge JSON is null."
    Assert-Condition ((Get-JsonField $Json "protocol") -eq "hyard_v2") "Expected protocol=hyard_v2."

    if (-not $AllowMissingCommand) {
        Assert-Condition ((Get-JsonField $Json "command") -eq $ExpectedCommand) "Expected command='$ExpectedCommand'."
    }
}

function Add-StepRecord {
    param(
        [string]$Name,
        [string[]]$Arguments,
        [int]$ExitCode,
        [string]$Raw,
        $Json
    )

    $script:Report.steps += [ordered]@{
        name       = $Name
        arguments  = @($Arguments)
        exit_code  = $ExitCode
        raw_stdout = $Raw
        json       = $Json
    }
}

function Invoke-BridgeStep {
    param(
        [string]$Name,
        [string[]]$Arguments,
        [string]$ExpectedCommand,
        [switch]$AllowMissingCommand,
        [scriptblock]$Validator
    )

    Write-Host ""
    Write-Host "== $Name =="
    Write-Host ("> " + $script:SwitchyardExe + " " + ($Arguments -join " "))

    $lines = & $script:SwitchyardExe @Arguments 2>&1 | ForEach-Object { $_.ToString() }
    $exitCode = $LASTEXITCODE
    $raw = ($lines -join [Environment]::NewLine).Trim()

    if ($raw) {
        Write-Host $raw
    }

    $json = Convert-BridgeOutput -Raw $raw -Label $Name
    Add-StepRecord -Name $Name -Arguments $Arguments -ExitCode $exitCode -Raw $raw -Json $json

    if ($exitCode -ne 0) {
        throw "$Name failed with exit code $exitCode.`nRaw:`n$raw"
    }

    Assert-BridgeEnvelope -Json $json -ExpectedCommand $ExpectedCommand -AllowMissingCommand:$AllowMissingCommand

    if ($Validator) {
        & $Validator $json
    }

    return $json
}

function Validate-CommonJobEnvelope {
    param(
        $Json,
        [string[]]$AllowedStatus
    )

    $status = [string](Get-JsonField $Json "status")
    Assert-Condition ($AllowedStatus -contains $status) "Unexpected status '$status'. Allowed: $($AllowedStatus -join ', ')"
    Assert-Condition (-not [string]::IsNullOrWhiteSpace([string](Get-JsonField $Json "job_id"))) "Expected job_id."
    Assert-Condition (-not [string]::IsNullOrWhiteSpace([string](Get-JsonField $Json "provider"))) "Expected provider."
    Assert-Condition (-not [string]::IsNullOrWhiteSpace([string](Get-JsonField $Json "message"))) "Expected message."
    $nextActions = @(Get-JsonField $Json "next_actions")
    Assert-Condition ($nextActions.Count -ge 0) "Expected next_actions array."
}

function Save-Report {
    param([bool]$Passed)

    $script:Report.finished_at = (Get-Date).ToString("o")
    $script:Report.summary.passed = $Passed

    $json = $script:Report | ConvertTo-Json -Depth 64
    Ensure-Directory -Path $script:ResolvedReportPath
    Set-Content -Path $script:ResolvedReportPath -Value $json -Encoding utf8
}

try {
    $script:SwitchyardExe = Resolve-SwitchyardExe -PreferredPath $SwitchyardExePath
    $script:ResolvedReportPath = if ($ReportPath) { $ReportPath } else { Default-ReportPath }
    $script:Report.switchyard_exe = $script:SwitchyardExe
    $script:Report.report_path = $script:ResolvedReportPath

    Write-Host "Starting HYARD host bridge smoke run"
    Write-Host "  provider:   $Provider"
    Write-Host "  task:       $Task"
    Write-Host "  exe:        $script:SwitchyardExe"
    Write-Host "  report:     $script:ResolvedReportPath"

    $help = Invoke-BridgeStep -Name "host help" -Arguments @("host", "help") -ExpectedCommand "" -AllowMissingCommand -Validator {
        param($json)
        Assert-Condition ((Get-JsonField $json "protocol") -eq "hyard_v2") "help must include protocol=hyard_v2."
        $commands = @(Get-JsonField $json "commands")
        Assert-Condition ($commands.Count -gt 0) "help must include commands."
        $commandNames = @($commands | ForEach-Object { $_.name })
        foreach ($required in @("/hyard:list", "/hyard:delegate", "/hyard:status", "/hyard:await", "/hyard:result", "/hyard:cancel")) {
            Assert-Condition ($commandNames -contains $required) "help missing command '$required'."
        }
    }

    $list = Invoke-BridgeStep -Name "host list" -Arguments @("host", "list") -ExpectedCommand "list" -Validator {
        param($json)
        $peers = @(Get-JsonField $json "peers")
        Assert-Condition ($peers.Count -ge 1) "list must include peers."
        $providers = @($peers | ForEach-Object { $_.provider })
        Assert-Condition ($providers -contains $Provider) "list did not include target provider '$Provider'."
    }

    $delegate = Invoke-BridgeStep -Name "host delegate" -Arguments @(
        "host", "delegate",
        "--provider", $Provider,
        "--task", $Task,
        "--wait-sec", $InitialWaitSec
    ) -ExpectedCommand "delegate" -Validator {
        param($json)
        Validate-CommonJobEnvelope -Json $json -AllowedStatus @("completed", "wait_timeout", "failed", "cancelled")
    }

    $jobId = [string](Get-JsonField $delegate "job_id")
    $delegateStatus = [string](Get-JsonField $delegate "status")

    if ($RequireWaitTimeout) {
        Assert-Condition ($delegateStatus -eq "wait_timeout") "RequireWaitTimeout was set, but delegate returned '$delegateStatus'."
    }

    $status = Invoke-BridgeStep -Name "host status" -Arguments @("host", "status", "--job-id", $jobId) -ExpectedCommand "status" -Validator {
        param($json)
        Validate-CommonJobEnvelope -Json $json -AllowedStatus @("queued", "running", "cancel_requested", "completed", "failed", "cancelled", "pending")
        Assert-Condition ((Get-JsonField $json "job_id") -eq $jobId) "status job_id mismatch."
    }

    $result = Invoke-BridgeStep -Name "host result" -Arguments @("host", "result", "--job-id", $jobId) -ExpectedCommand "result" -Validator {
        param($json)
        Validate-CommonJobEnvelope -Json $json -AllowedStatus @("queued", "running", "cancel_requested", "completed", "failed", "cancelled", "pending")
        Assert-Condition ((Get-JsonField $json "job_id") -eq $jobId) "result job_id mismatch."
    }

    $await = $null
    $postAwaitStatus = $null
    $postAwaitResult = $null

    $statusAfterDelegate = [string](Get-JsonField $status "status")
    $resultAfterDelegate = [string](Get-JsonField $result "status")
    $resultReadyAfterDelegate = [bool](Get-JsonField $result "result_ready")

    $needsAwait =
        (Is-ActiveStatus $statusAfterDelegate) -or
        ((Is-ActiveStatus $resultAfterDelegate) -and (-not $resultReadyAfterDelegate))

    if ($RequireAwait) {
        $needsAwait = $true
    }

    if ($needsAwait) {
        Write-Host ""
        if ($RequireAwait -and -not ((Is-ActiveStatus $statusAfterDelegate) -or ((Is-ActiveStatus $resultAfterDelegate) -and (-not $resultReadyAfterDelegate)))) {
            Write-Host "RequireAwait was set. Continuing with host await on the same job_id."
        }
        else {
            Write-Host "Delegate/status/result indicate an active background job. Continuing with host await."
        }
        $await = Invoke-BridgeStep -Name "host await" -Arguments @(
            "host", "await",
            "--job-id", $jobId,
            "--timeout-sec", $AwaitTimeoutSec
        ) -ExpectedCommand "await" -Validator {
            param($json)
            Validate-CommonJobEnvelope -Json $json -AllowedStatus @("completed", "wait_timeout", "failed", "cancelled")
            Assert-Condition ((Get-JsonField $json "job_id") -eq $jobId) "await job_id mismatch."
        }

        $postAwaitStatus = Invoke-BridgeStep -Name "host status (post-await)" -Arguments @("host", "status", "--job-id", $jobId) -ExpectedCommand "status" -Validator {
            param($json)
            Validate-CommonJobEnvelope -Json $json -AllowedStatus @("queued", "running", "cancel_requested", "completed", "failed", "cancelled", "pending")
            Assert-Condition ((Get-JsonField $json "job_id") -eq $jobId) "post-await status job_id mismatch."
        }

        $postAwaitResult = Invoke-BridgeStep -Name "host result (post-await)" -Arguments @("host", "result", "--job-id", $jobId) -ExpectedCommand "result" -Validator {
            param($json)
            Validate-CommonJobEnvelope -Json $json -AllowedStatus @("queued", "running", "cancel_requested", "completed", "failed", "cancelled", "pending")
            Assert-Condition ((Get-JsonField $json "job_id") -eq $jobId) "post-await result job_id mismatch."
        }
    }

    if ($RequireAwait) {
        Assert-Condition ($null -ne $await) "RequireAwait was set, but no await step was executed."
    }

    $cancel = $null
    if (-not $SkipCancel) {
        $cancel = Invoke-BridgeStep -Name "host cancel" -Arguments @("host", "cancel", "--job-id", $jobId) -ExpectedCommand "cancel" -Validator {
            param($json)
            Validate-CommonJobEnvelope -Json $json -AllowedStatus @("cancel_requested", "cancelled", "completed", "failed")
            Assert-Condition ((Get-JsonField $json "job_id") -eq $jobId) "cancel job_id mismatch."
        }
    }

    $finalStatusJson = if ($postAwaitStatus) { $postAwaitStatus } else { $status }
    $finalResultJson = if ($postAwaitResult) { $postAwaitResult } else { $result }
    $finalJobStatus = [string](Get-JsonField $finalStatusJson "status")
    $finalResultStatus = [string](Get-JsonField $finalResultJson "status")
    $terminalReached = (Is-TerminalStatus $finalJobStatus) -or (Is-TerminalStatus $finalResultStatus)

    if ($RequireTerminal) {
        Assert-Condition $terminalReached "RequireTerminal was set, but job never reached a terminal state."
    }

    $script:Report.summary = [ordered]@{
        passed               = $true
        provider             = $Provider
        job_id               = $jobId
        delegate_status      = $delegateStatus
        status_after_delegate = $statusAfterDelegate
        result_after_delegate = $resultAfterDelegate
        require_wait_timeout = [bool]$RequireWaitTimeout
        require_await        = [bool]$RequireAwait
        await_status         = if ($await) { [string](Get-JsonField $await "status") } else { $null }
        final_status         = $finalJobStatus
        final_result_status  = $finalResultStatus
        cancel_status        = if ($cancel) { [string](Get-JsonField $cancel "status") } else { $null }
        terminal_reached     = $terminalReached
        result_ready         = [bool](Get-JsonField $finalResultJson "result_ready")
    }

    Save-Report -Passed $true

    Write-Host ""
    Write-Host "HYARD host bridge smoke run PASSED."
    Write-Host "  job_id:          $jobId"
    Write-Host "  delegate_status: $delegateStatus"
    Write-Host "  final_status:    $finalJobStatus"
    Write-Host "  result_status:   $finalResultStatus"
    Write-Host "  report:          $script:ResolvedReportPath"
}
catch {
    $script:Report.summary = [ordered]@{
        passed        = $false
        provider      = $Provider
        error         = $_.Exception.Message
        failed_step   = if ($script:Report.steps.Count -gt 0) { $script:Report.steps[-1].name } else { $null }
    }
    Save-Report -Passed $false

    Write-Error $_.Exception.Message
    Write-Error "HYARD host bridge smoke run FAILED. Report: $script:ResolvedReportPath"
    exit 1
}
