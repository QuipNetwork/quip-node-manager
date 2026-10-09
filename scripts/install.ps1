# SPDX-License-Identifier: AGPL-3.0-or-later
# Quip Node Manager installer for Windows.
# Usage: irm https://gitlab.com/quip.network/quip-node-manager/-/raw/main/scripts/install.ps1 | iex

[Diagnostics.CodeAnalysis.SuppressMessageAttribute(
    'PSUseDeclaredVarsMoreThanAssignments', 'ExpectedSigner',
    Justification = 'This task defines the signer pin before download verification is connected.')]
[Diagnostics.CodeAnalysis.SuppressMessageAttribute(
    'PSUseDeclaredVarsMoreThanAssignments', 'ReleaseKeyFingerprint',
    Justification = 'The release key pin is used once Task 6 connects download verification.')]
param()

$ErrorActionPreference = "Stop"

$Repo = "quip.network%2Fquip-node-manager"
$Api = "https://gitlab.com/api/v4/projects/$Repo/releases"
$Artifact = "quip-node-manager-windows-x86_64.exe"

# The common name on the Authenticode certificate, as SSL.com issued it. Keep
# it equal to EXPECTED_CN_PROD in scripts/verify-windows-signing.sh: CI refuses
# to publish a binary whose signer differs, and this script refuses to install
# one. scripts/tests/install.Tests.ps1 checks that the two agree.
$ExpectedSigner = "HADAMARD GATE INCORPORATED"

# The OpenPGP key that signs SHA256SUMS. Keep it equal to
# RELEASE_KEY_FINGERPRINT in scripts/install.sh; the same test checks this one.
$ReleaseKeyFingerprint = "A63860E21E7070C2C26FDA5DC85BEAB01AD9FEE3"

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

# True when the subject string has a top-level CN attribute. Walks the string
# so a comma or "CN=" inside a quoted value does not count; both .NET and
# CryptoAPI render such values in double quotes.
function Test-SubjectHasCN([string]$Subject) {
    $inQuotes = $false
    $start = 0
    for ($i = 0; $i -le $Subject.Length; $i++) {
        $atEnd = ($i -eq $Subject.Length)
        if (-not $atEnd -and $Subject[$i] -eq '"') { $inQuotes = -not $inQuotes; continue }
        if ($atEnd -or ($Subject[$i] -eq ',' -and -not $inQuotes)) {
            if ($Subject.Substring($start, $i - $start).Trim() -clike 'CN=*') { return $true }
            $start = $i + 1
        }
    }
    return $false
}

# The Windows analogue of the key pin in install.sh: Windows itself checks the
# chain, and this checks that the leaf names the expected publisher.
function Confirm-AuthenticodeSigner([string]$Path, [string]$ExpectedCN) {
    $sig = Get-AuthenticodeSignature -LiteralPath $Path
    if ("$($sig.Status)" -cne 'Valid') {
        throw "The Authenticode signature on $Path is $($sig.Status), not Valid."
    }
    $subject = "$($sig.SignerCertificate.Subject)"
    # Ask the certificate for its common name instead of parsing the Subject
    # string, so quoting, escaping and newlines in other RDNs never reach the
    # comparison. SimpleName falls back to OU, O or email when the leaf has
    # no CN, so also require that a CN exists.
    $cn = "$($sig.SignerCertificate.GetNameInfo('SimpleName', $false))"
    $hasCN = Test-SubjectHasCN $subject
    if (-not $hasCN -or $cn -cne $ExpectedCN) {
        throw "The binary is signed by '$subject', not by $ExpectedCN."
    }
}

# Mirrors the gpg branch of install.sh. A throwaway --homedir rather than
# --keyring: keyboxd ignores --keyring, and a separate home is the one
# isolation every gpg version honours.
function Confirm-SumsSignature(
    [string]$SumsPath, [string]$SigPath, [string]$KeyPath, [string]$Fingerprint
) {
    $gpgHome = Join-Path ([System.IO.Path]::GetTempPath()) `
        ("quip-gnupg-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $gpgHome | Out-Null
    try {
        & gpg --homedir $gpgHome --batch --quiet --import $KeyPath
        if ($LASTEXITCODE -ne 0) { throw "Could not read the release signing key." }

        $listing = @(& gpg --homedir $gpgHome --batch --list-keys --with-colons)
        if ($LASTEXITCODE -ne 0) { throw "Could not read the release key back from gpg." }
        # The fingerprint below pins the first key. A key file that carries a
        # second primary key would let gpg accept a signature from that one,
        # so the pinned key must be the only primary key gpg can use.
        $primaries = @($listing | Where-Object { "$_" -clike 'pub:*' })
        if ($primaries.Count -ne 1) {
            throw "The release key file holds $($primaries.Count) primary keys, expected 1."
        }
        $fprLine = @($listing) | Where-Object { "$_" -like 'fpr:*' } | Select-Object -First 1
        $found = if ($fprLine) { ("$fprLine" -split ':')[9] } else { $null }
        if (-not $found) { throw "gpg reported no fingerprint for the release key." }
        if ($found -cne $Fingerprint) {
            throw "Release key fingerprint is $found, expected $Fingerprint."
        }

        & gpg --homedir $gpgHome --batch --verify $SigPath $SumsPath
        if ($LASTEXITCODE -ne 0) { throw "Signature on SHA256SUMS is not valid." }
    } finally {
        # gpg 2.4+ starts keyboxd and gpg-agent against the home; stop them
        # before the directory goes, or they outlive it.
        if (Get-Command gpgconf -ErrorAction SilentlyContinue) {
            & gpgconf --homedir $gpgHome --kill all | Out-Null
        }
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $gpgHome
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
