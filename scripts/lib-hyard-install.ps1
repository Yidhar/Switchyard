Set-StrictMode -Version Latest

$script:Utf8NoBom = New-Object System.Text.UTF8Encoding($false)

function Write-Utf8NoBom([string]$Path, [string]$Content) {
    $parent = Split-Path -Parent $Path
    if ($parent -and -not (Test-Path $parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    [System.IO.File]::WriteAllText($Path, $Content, $script:Utf8NoBom)
}

function Get-NormalizedPath([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path)) {
        return $null
    }

    try {
        return [System.IO.Path]::GetFullPath($Path).TrimEnd('\')
    } catch {
        return $Path.Trim().TrimEnd('\')
    }
}

function Get-PathEntries([string]$PathValue = $env:PATH) {
    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return @()
    }

    return @(
        $PathValue.Split(';') |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
            ForEach-Object { Get-NormalizedPath $_ } |
            Where-Object { $_ }
    )
}

function Test-PathEntryPresent([string]$Needle, [string[]]$Haystack) {
    $normalizedNeedle = Get-NormalizedPath $Needle
    if (-not $normalizedNeedle) {
        return $false
    }

    foreach ($entry in $Haystack) {
        if ($entry -and $entry.Equals($normalizedNeedle, [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }

    return $false
}

function Find-SwitchyardRepoBinary([string]$RepoRoot) {
    $candidates = @(
        (Join-Path $RepoRoot "target\debug\switchyard.exe"),
        (Join-Path $RepoRoot "target\release\switchyard.exe")
    )

    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) {
            return (Resolve-Path $candidate).Path
        }
    }

    return $null
}

function Get-PreferredSwitchyardShimDir {
    $pathEntries = Get-PathEntries
    $preferred = @(
        (Join-Path $env:USERPROFILE ".cargo\bin"),
        (Join-Path $env:APPDATA "npm"),
        (Join-Path $env:USERPROFILE ".switchyard\bin")
    ) | Where-Object { $_ }

    foreach ($candidate in $preferred) {
        if (Test-PathEntryPresent $candidate $pathEntries) {
            return $candidate
        }
    }

    return (Join-Path $env:USERPROFILE ".switchyard\bin")
}

function Ensure-PathEntry([string]$Directory) {
    $normalizedDir = Get-NormalizedPath $Directory
    if (-not $normalizedDir) {
        return
    }

    $currentEntries = Get-PathEntries
    if (Test-PathEntryPresent $normalizedDir $currentEntries) {
        return
    }

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $userEntries = Get-PathEntries $userPath
    if (-not (Test-PathEntryPresent $normalizedDir $userEntries)) {
        $newUserPath = if ([string]::IsNullOrWhiteSpace($userPath)) {
            $normalizedDir
        } else {
            "$userPath;$normalizedDir"
        }
        [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
    }

    $env:PATH = "$normalizedDir;$env:PATH"
}

function Ensure-SwitchyardShim([string]$RepoRoot) {
    $binary = Find-SwitchyardRepoBinary $RepoRoot
    if (-not $binary) {
        return $null
    }

    $shimDir = Get-PreferredSwitchyardShimDir
    if (-not (Test-Path $shimDir)) {
        New-Item -ItemType Directory -Path $shimDir -Force | Out-Null
    }

    $repoRootResolved = (Resolve-Path $RepoRoot).Path
    $shimPath = Join-Path $shimDir "switchyard.cmd"
    $shimContent = @"
@echo off
setlocal
set "SWITCHYARD_REPO=$repoRootResolved"
set "SWITCHYARD_BIN=%SWITCHYARD_REPO%\target\debug\switchyard.exe"
if exist "%SWITCHYARD_BIN%" goto run
set "SWITCHYARD_BIN=%SWITCHYARD_REPO%\target\release\switchyard.exe"
if exist "%SWITCHYARD_BIN%" goto run
echo switchyard binary not found under "%SWITCHYARD_REPO%\target\debug" or "%SWITCHYARD_REPO%\target\release". 1>&2
exit /b 1
:run
"%SWITCHYARD_BIN%" %*
exit /b %ERRORLEVEL%
"@

    Write-Utf8NoBom -Path $shimPath -Content $shimContent
    Ensure-PathEntry $shimDir

    return [PSCustomObject]@{
        Command = "switchyard"
        ShimPath = $shimPath
        ShimDir = $shimDir
        BinaryPath = $binary
    }
}

function Resolve-SwitchyardCommand([string]$RepoRoot) {
    $command = Get-Command switchyard -ErrorAction SilentlyContinue
    if ($command) {
        return [PSCustomObject]@{
            Command = "switchyard"
            Source = "path"
            Path = $command.Path
            ShimCreated = $false
        }
    }

    $shim = Ensure-SwitchyardShim $RepoRoot
    if ($shim) {
        return [PSCustomObject]@{
            Command = $shim.Command
            Source = "shim"
            Path = $shim.ShimPath
            ShimCreated = $true
        }
    }

    $binary = Find-SwitchyardRepoBinary $RepoRoot
    if ($binary) {
        $commandText = if ($binary -match '\s') {
            '"' + $binary + '"'
        } else {
            $binary
        }
        return [PSCustomObject]@{
            Command = $commandText
            Source = "repo-binary"
            Path = $binary
            ShimCreated = $false
        }
    }

    Write-Warning "switchyard binary not found on PATH and no repo build exists under target\\{debug,release}; installed instructions will keep the generic 'switchyard' command."
    return [PSCustomObject]@{
        Command = "switchyard"
        Source = "generic"
        Path = $null
        ShimCreated = $false
    }
}
