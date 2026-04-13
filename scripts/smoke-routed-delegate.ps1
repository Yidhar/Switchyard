param(
    [string]$CoreProvider = "codex",
    [string]$PeerProvider = "claude",
    [string]$PeerRole = "",
    [string]$SwitchyardExePath,
    [string]$ReportPath,
    [switch]$SkipCheck
)

$ErrorActionPreference = "Stop"

$script:RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$script:Report = [ordered]@{
    smoke_protocol = "hyard_routed_delegate_smoke_v1"
    started_at     = (Get-Date).ToString("o")
    core_provider  = $CoreProvider
    peer_provider  = $PeerProvider
    peer_role      = $PeerRole
    steps          = @()
    summary        = [ordered]@{}
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
    $reportDir = Join-Path $script:RepoRoot ".switchyard\smoke\routed-delegate"
    if (-not (Test-Path $reportDir)) {
        New-Item -ItemType Directory -Path $reportDir -Force | Out-Null
    }
    return Join-Path $reportDir ("{0}-{1}-to-{2}.json" -f $timestamp, $CoreProvider, $PeerProvider)
}

function Assert-Condition {
    param(
        $Condition,
        [string]$Message
    )

    if (-not [bool]$Condition) {
        throw $Message
    }
}

function Convert-JsonMaybe {
    param([string]$Raw)

    if ([string]::IsNullOrWhiteSpace($Raw)) {
        return $null
    }

    if ((Get-Command ConvertFrom-Json).Parameters.ContainsKey("Depth")) {
        return $Raw | ConvertFrom-Json -Depth 64
    }

    return $Raw | ConvertFrom-Json
}

function Get-DefaultPeerRole {
    param([string]$Provider)

    switch ($Provider) {
        "claude" { return "reviewer" }
        "gemini" { return "analyst" }
        "codex"  { return "worker" }
        default  { return "worker" }
    }
}

function Read-JsonLines {
    param([string]$Path)

    if (-not (Test-Path $Path)) {
        return @()
    }

    $objects = New-Object System.Collections.Generic.List[object]
    foreach ($line in Get-Content $Path) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        [void]$objects.Add((Convert-JsonMaybe -Raw $line))
    }
    return @($objects.ToArray())
}

function Get-FinalTurnStates {
    param([object[]]$TurnRecords)

    $byId = @{}
    foreach ($turn in $TurnRecords) {
        if ($null -ne $turn -and $turn.turn_id) {
            $byId[[string]$turn.turn_id] = $turn
        }
    }

    return @(
        $byId.Values |
            Sort-Object `
                @{ Expression = { Get-DateSortKey $_.started_at } }, `
                @{ Expression = { [string]$_.turn_id } }
    )
}

function Get-DateSortKey {
    param($Value)

    if ($null -eq $Value -or [string]::IsNullOrWhiteSpace([string]$Value)) {
        return [long]0
    }

    try {
        return ([datetimeoffset]$Value).UtcDateTime.Ticks
    }
    catch {
        return [long]0
    }
}

function Get-NewSessionDirectory {
    param(
        [string]$SessionRoot,
        [string[]]$BeforeSessionNames,
        [datetime]$StartedAt
    )

    if (-not (Test-Path $SessionRoot)) {
        return $null
    }

    $dirs = @(Get-ChildItem $SessionRoot -Directory)
    $newDirs = @(
        $dirs | Where-Object {
            $BeforeSessionNames -notcontains $_.Name
        }
    )

    if ($newDirs.Count -eq 1) {
        return $newDirs[0]
    }

    if ($newDirs.Count -gt 1) {
        return ($newDirs | Sort-Object LastWriteTime -Descending | Select-Object -First 1)
    }

    $recent = @(
        $dirs | Where-Object {
            $_.LastWriteTime -ge $StartedAt.AddSeconds(-5)
        }
    )

    if ($recent.Count -gt 0) {
        return ($recent | Sort-Object LastWriteTime -Descending | Select-Object -First 1)
    }

    return $null
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
    $stage = "resolve-binary"
    $script:SwitchyardExe = Resolve-SwitchyardExe -PreferredPath $SwitchyardExePath
    $script:ResolvedReportPath = if ($ReportPath) { $ReportPath } else { Default-ReportPath }

    if ([string]::IsNullOrWhiteSpace($PeerRole)) {
        $PeerRole = Get-DefaultPeerRole -Provider $PeerProvider
    }

    $script:Report.switchyard_exe = $script:SwitchyardExe
    $script:Report.report_path = $script:ResolvedReportPath
    $script:Report.peer_role = $PeerRole

    $sessionRoot = Join-Path $script:RepoRoot ".switchyard\sessions"
    if (-not (Test-Path $sessionRoot)) {
        New-Item -ItemType Directory -Path $sessionRoot -Force | Out-Null
    }

    $beforeSessions = @(
        Get-ChildItem $sessionRoot -Directory -ErrorAction SilentlyContinue |
            Select-Object -ExpandProperty Name
    )

    $timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $runDir = Join-Path $script:RepoRoot (".switchyard\smoke\routed-delegate\run-" + $timestamp + "-" + $CoreProvider + "-to-" + $PeerProvider)
    New-Item -ItemType Directory -Path $runDir -Force | Out-Null

    $stdoutPath = Join-Path $runDir "stdout.txt"
    $stderrPath = Join-Path $runDir "stderr.txt"
    $checkPath = Join-Path $runDir "check.json"

    $peerTask = "What is 2 + 2? Answer with exactly one character."
    $peerAnswer = "4"
    $expectedFinal = "final-ok: ${PeerProvider}:$peerAnswer"
    $prompt = @"
Do not answer directly.
First emit a valid Switchyard delegate sentinel JSON block with exactly one request using this payload:
{"type":"delegate","requests":[{"id":"smoke-$PeerProvider","provider":"$PeerProvider","role":"$PeerRole","task":"$peerTask","write_access":false,"timeout_sec":120}]}
Do not include any surrounding explanation before or after the sentinel block.
After the delegate result comes back, respond with exactly: $expectedFinal
"@

    $script:Report.prompt = $prompt
    $script:Report.peer_task = $peerTask
    $script:Report.expected_peer_answer = $peerAnswer
    $script:Report.expected_final = $expectedFinal
    $script:Report.run_dir = $runDir

    Write-Host "Starting routed delegate smoke"
    Write-Host "  core:         $CoreProvider"
    Write-Host "  peer:         $PeerProvider ($PeerRole)"
    Write-Host "  exe:          $script:SwitchyardExe"
    Write-Host "  report:       $script:ResolvedReportPath"

    if (-not $SkipCheck) {
        $stage = "check"
        Write-Host ""
        Write-Host "== switchyard check --json =="
        & $script:SwitchyardExe check --json 1> $checkPath
        $checkExitCode = $LASTEXITCODE
        $checkRaw = if (Test-Path $checkPath) { Get-Content $checkPath -Raw } else { "" }
        $checkJson = Convert-JsonMaybe -Raw $checkRaw
        $script:Report.steps += [ordered]@{
            name       = "check"
            exit_code  = $checkExitCode
            raw_stdout = $checkRaw
            json       = $checkJson
        }
        Assert-Condition ($checkExitCode -eq 0) "switchyard check --json failed with exit code $checkExitCode."
        $providerStatus = @($checkJson.providers | Where-Object { $_.provider -eq $PeerProvider })
        Assert-Condition ($providerStatus.Count -eq 1) "check --json did not include peer provider '$PeerProvider'."
        Assert-Condition ($providerStatus[0].status -eq "ready") "peer provider '$PeerProvider' is not ready: $($providerStatus[0].status)"
    }

    Write-Host ""
    Write-Host "== switchyard run =="
    Write-Host ("> " + $script:SwitchyardExe + " run --provider " + $CoreProvider + " --message <delegate-smoke-prompt> --cwd " + $script:RepoRoot)

    $stage = "run"
    $startedAt = Get-Date
    & $script:SwitchyardExe run --provider $CoreProvider --message $prompt --cwd $script:RepoRoot 1> $stdoutPath 2> $stderrPath
    $runExitCode = $LASTEXITCODE
    $stdout = if (Test-Path $stdoutPath) { Get-Content $stdoutPath -Raw } else { "" }
    $stderr = if (Test-Path $stderrPath) { Get-Content $stderrPath -Raw } else { "" }

    $script:Report.steps += [ordered]@{
        name          = "run"
        exit_code     = $runExitCode
        stdout_path   = $stdoutPath
        stderr_path   = $stderrPath
        raw_stdout    = $stdout
        raw_stderr    = $stderr
    }

    if ($stdout) {
        Write-Host ($stdout.Trim())
    }
    if ($stderr) {
        Write-Host "[stderr]"
        Write-Host $stderr.Trim()
    }

    Assert-Condition ($runExitCode -eq 0) "switchyard run failed with exit code $runExitCode."

    $stage = "locate-session"
    $sessionDirCandidates = @(Get-NewSessionDirectory -SessionRoot $sessionRoot -BeforeSessionNames $beforeSessions -StartedAt $startedAt)
    $script:Report.session_candidates = @($sessionDirCandidates | ForEach-Object { $_.FullName })
    $sessionDir = @($sessionDirCandidates | Select-Object -First 1)
    if ($sessionDir.Count -gt 0) {
        $sessionDir = $sessionDir[0]
    }
    Assert-Condition ($null -ne $sessionDir) "Could not locate new session directory after routed delegate run."

    $stage = "load-session-data"
    $stage = "load-turns"
    $turnRecords = Read-JsonLines -Path (Join-Path $sessionDir.FullName "turns.jsonl")
    $stage = "load-artifacts"
    $artifactRecords = Read-JsonLines -Path (Join-Path $sessionDir.FullName "artifacts.jsonl")
    $stage = "load-events"
    $eventRecords = Read-JsonLines -Path (Join-Path $sessionDir.FullName "events.jsonl")

    $stage = "validate-turns"
    $turns = Get-FinalTurnStates -TurnRecords $turnRecords
    $coreTurns = @($turns | Where-Object { $_.provider -eq $CoreProvider -and $_.role -eq "core" })
    $delegateTurns = @($turns | Where-Object { $_.origin -eq "delegate" -and $_.provider -eq $PeerProvider })

    Assert-Condition ($turns.Count -ge 3) "Expected at least 3 final turn states, got $($turns.Count)."
    Assert-Condition ($coreTurns.Count -ge 2) "Expected at least 2 core turns, got $($coreTurns.Count)."
    Assert-Condition ($delegateTurns.Count -ge 1) "Expected at least 1 delegate turn for '$PeerProvider'."

    $initialCore = $coreTurns[0]
    $delegateTurn = $delegateTurns[-1]
    $finalCore = $coreTurns[-1]

    Assert-Condition ($initialCore.status -eq "completed") "Initial core turn did not complete successfully."
    Assert-Condition ($delegateTurn.status -eq "completed") "Delegate turn did not complete successfully: $($delegateTurn.status)"
    Assert-Condition ($finalCore.status -eq "completed") "Final core turn did not complete successfully."
    Assert-Condition (($delegateTurn.provider_response | Out-String).Trim() -eq $peerAnswer) "Delegate response mismatch. Expected '$peerAnswer', got '$($delegateTurn.provider_response)'."
    Assert-Condition (($finalCore.provider_response | Out-String).Trim() -eq $expectedFinal) "Final response mismatch. Expected '$expectedFinal', got '$($finalCore.provider_response)'."
    Assert-Condition (($stdout | Out-String).Trim() -eq $expectedFinal) "CLI stdout mismatch. Expected '$expectedFinal', got '$($stdout.Trim())'."
    Assert-Condition (($initialCore.provider_response | Out-String) -match "SWITCHYARD_JSON_BEGIN") "Initial core turn did not emit a delegate sentinel block."
    Assert-Condition (($finalCore.user_message | Out-String) -match "delegate_result") "Final core turn input did not include delegate_result."

    $delegateArtifacts = @($artifactRecords | Where-Object { $_.turn_id -eq $delegateTurn.turn_id })
    Assert-Condition ($delegateArtifacts.Count -ge 1) "Expected delegate artifacts for turn $($delegateTurn.turn_id)."

    $peerEvents = @($eventRecords | Where-Object { $_.provider -eq $PeerProvider })
    Assert-Condition ($peerEvents.Count -ge 1) "Expected peer events for provider '$PeerProvider'."

    $stage = "build-summary"
    $script:Report.summary = [ordered]@{
        passed               = $true
        stage                = $stage
        session_dir          = $sessionDir.FullName
        session_id           = $sessionDir.Name
        turn_count           = $turns.Count
        core_turn_count      = $coreTurns.Count
        delegate_turn_count  = $delegateTurns.Count
        event_count          = $eventRecords.Count
        delegate_event_count = $peerEvents.Count
        delegate_artifacts   = $delegateArtifacts.Count
        delegate_turn_id     = $delegateTurn.turn_id
        final_turn_id        = $finalCore.turn_id
        delegate_response    = $delegateTurn.provider_response
        final_response       = $finalCore.provider_response
    }

    $stage = "save-report"
    Save-Report -Passed $true

    $stage = "done"
    Write-Host ""
    Write-Host "Routed delegate smoke PASSED."
    Write-Host "  session:       $($sessionDir.Name)"
    Write-Host "  delegate turn: $($delegateTurn.turn_id)"
    Write-Host "  final response:$($finalCore.provider_response)"
    Write-Host "  report:        $script:ResolvedReportPath"
}
catch {
    $script:Report.summary = [ordered]@{
        passed      = $false
        error       = $_.Exception.Message
        stage       = $stage
        failed_step = if ($script:Report.steps.Count -gt 0) { $script:Report.steps[-1].name } else { $null }
    }
    Save-Report -Passed $false

    Write-Host ("ERROR: " + [string]$_.Exception.Message)
    Write-Host "Routed delegate smoke FAILED. Report: $script:ResolvedReportPath"
    exit 1
}
