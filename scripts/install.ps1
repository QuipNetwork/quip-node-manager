# SPDX-License-Identifier: AGPL-3.0-or-later
# Quip Node Manager installer for Windows.
# Usage: irm https://gitlab.com/piqued/quip-node-manager/-/raw/main/scripts/install.ps1 | iex

$ErrorActionPreference = "Stop"

$Repo = "piqued%2Fquip-node-manager"
$Api = "https://gitlab.com/api/v4/projects/$Repo/releases"

function Info($msg)  { Write-Host "> $msg" -ForegroundColor Cyan }
function Error($msg) { Write-Host "! $msg" -ForegroundColor Red; exit 1 }

# ── Fetch latest release tag ────────────────────────────────────────────────
Info "Fetching latest release..."
try {
    $releases = Invoke-RestMethod -Uri $Api -UseBasicParsing
} catch {
    Error "Failed to query releases: $_"
}

if (-not $releases -or $releases.Count -eq 0) {
    Error "No releases found."
}

$Tag = $releases[0].tag_name
if (-not $Tag) { Error "Could not determine latest release tag." }
Info "Latest release: $Tag"

# ── Build artifact URL ──────────────────────────────────────────────────────
$Base = "https://gitlab.com/piqued/quip-node-manager/-/jobs/artifacts/$Tag/raw/dist"
$Artifact = "quip-node-manager-windows-x86_64.exe"
$Url = "$Base/$Artifact`?job=build-windows-x86_64"

# ── Download ────────────────────────────────────────────────────────────────
$InstallDir = Join-Path $env:LOCALAPPDATA "QuipNodeManager"
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}
$Dest = Join-Path $InstallDir "quip-node-manager.exe"

Info "Downloading $Artifact..."
try {
    Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing
} catch {
    Error "Download failed: $_"
}

# ── Add to PATH (user scope) ───────────────────────────────────────────────
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
