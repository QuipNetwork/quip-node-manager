# SPDX-License-Identifier: AGPL-3.0-or-later
# Quip Node Manager installer for Windows.
# Usage: irm https://gitlab.com/quip.network/quip-node-manager/-/raw/main/scripts/install.ps1 | iex

$ErrorActionPreference = "Stop"

$Repo = "quip.network%2Fquip-node-manager"
$Api = "https://gitlab.com/api/v4/projects/$Repo/releases"

function Info($msg)  { Write-Host "> $msg" -ForegroundColor Cyan }
# throw, not exit. Under `irm | iex` the script runs inside the user's own
# session, and exit ends that session before the message can be read.
function Fail($msg) {
    Write-Host "! $msg" -ForegroundColor Red
    throw $msg
}

# -- Helpers. scripts/tests/install.Tests.ps1 loads these without running the
# -- installer, so keep every function free of side effects on the session.

function Get-ReleaseFile([string]$Url, [string]$OutFile, [string]$What) {
    try {
        Invoke-WebRequest -Uri $Url -OutFile $OutFile -UseBasicParsing
    } catch {
        throw "Could not download ${What}: $($_.Exception.Message)"
    }
}

# The SHA-256 digest SHA256SUMS lists for $Name, lowercase, or $null when the
# file has no entry. sha256sum writes "<hex>  <name>", or "<hex> *<name>" in
# binary mode; both forms are accepted, the same as install.sh.
function Get-ExpectedSha256([string]$SumsPath, [string]$Name) {
    foreach ($line in Get-Content -LiteralPath $SumsPath) {
        if ($line -match '^([0-9A-Fa-f]{64})\s+\*?(\S+)\s*$' -and $Matches[2] -ceq $Name) {
            return $Matches[1].ToLowerInvariant()
        }
    }
    return $null
}

function Confirm-FileSha256([string]$Path, [string]$SumsPath, [string]$Name) {
    $expected = Get-ExpectedSha256 -SumsPath $SumsPath -Name $Name
    if (-not $expected) { throw "SHA256SUMS lists no entry for $Name." }
    $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -cne $expected) {
        throw "Checksum mismatch for $Name. The download is not the released file."
    }
}

# -- Fetch latest release tag ------------------------------------------------
Info "Fetching latest release..."
try {
    $releases = Invoke-RestMethod -Uri $Api -UseBasicParsing
} catch {
    Fail "Failed to query releases: $_"
}

if (-not $releases -or $releases.Count -eq 0) {
    Fail "No releases found."
}

$Tag = $releases[0].tag_name
if (-not $Tag) { Fail "Could not determine latest release tag." }
Info "Latest release: $Tag"

# -- Build artifact URL ------------------------------------------------------
$Base = "https://gitlab.com/quip.network/quip-node-manager/-/jobs/artifacts/$Tag/raw/dist"
$Artifact = "quip-node-manager-windows-x86_64.exe"
$Url = "$Base/$Artifact`?job=build-windows-x86_64"

# -- Download ----------------------------------------------------------------
$InstallDir = Join-Path $env:LOCALAPPDATA "QuipNodeManager"
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}
$Dest = Join-Path $InstallDir "quip-node-manager.exe"

Info "Downloading $Artifact..."
try {
    Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing
} catch {
    Fail "Download failed: $_"
}

# -- Add to PATH (user scope) -----------------------------------------------
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$InstallDir*") {
    Info "Adding $InstallDir to user PATH..."
    [Environment]::SetEnvironmentVariable(
        "Path", "$UserPath;$InstallDir", "User"
    )
    $env:Path = "$env:Path;$InstallDir"
}

Info "Installed to $Dest"
Info ""
Info "Windows SmartScreen may block the first launch."
Info "If you see a warning, click 'More info' then 'Run anyway'."
Info ""
Info "Run: quip-node-manager"
Info "Done."
