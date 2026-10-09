# SPDX-License-Identifier: AGPL-3.0-or-later
# Unit tests for the verification helpers in scripts/install.ps1.
# Run with: make test-powershell

BeforeAll {
    $script:ScriptsDir = Split-Path -Parent $PSScriptRoot
    $script:ScriptPath = Join-Path $script:ScriptsDir 'install.ps1'
    $script:ScriptText = Get-Content -LiteralPath $script:ScriptPath -Raw

    $tokens = $null
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile(
        $script:ScriptPath, [ref]$tokens, [ref]$errors)
    $script:ParseErrors = @($errors)

    # Load only the function definitions. Dot-sourcing the file would run the
    # installer.
    $functions = $ast.FindAll(
        { $args[0] -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $false)
    foreach ($f in $functions) { . ([scriptblock]::Create($f.Extent.Text)) }

    $script:Artifact = 'quip-node-manager-windows-x86_64.exe'
    $script:Work = Join-Path ([System.IO.Path]::GetTempPath()) ("install-tests-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $script:Work | Out-Null

    # A fixture binary and the digest sha256sum would print for it.
    $script:Exe = Join-Path $script:Work $script:Artifact
    [System.IO.File]::WriteAllBytes($script:Exe, [byte[]](0x4D, 0x5A, 1, 2, 3))
    $script:ExeHash = (Get-FileHash -LiteralPath $script:Exe -Algorithm SHA256).Hash.ToLowerInvariant()

    function script:Write-SumsFile([string[]]$Lines) {
        $path = Join-Path $script:Work 'SHA256SUMS'
        Set-Content -LiteralPath $path -Value $Lines
        return $path
    }
}

AfterAll {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $script:Work
}

Describe 'install.ps1 as a file' {
    It 'downloads the exe from the signing job, not the build job' {
        $script:ScriptText | Should -Match '\?job=sign-windows-x86_64'
        $script:ScriptText | Should -Not -Match 'job=build-windows-x86_64'
    }
    It 'removes temporary directories by literal path, so brackets in TEMP do not defeat cleanup' {
        $removals = @($script:ScriptText -split "`n" | Where-Object { $_ -match '\bRemove-Item\b' })
        $removals.Count | Should -Be 2
        foreach ($line in $removals) { $line | Should -Match '-LiteralPath\s' }
    }
    It 'parses without errors' {
        $script:ParseErrors.Count | Should -Be 0
    }
    It 'never calls exit, which under irm | iex would close the user session' {
        $script:ScriptText | Should -Not -Match '(?m)^\s*exit\b'
        $script:ScriptText | Should -Not -Match ';\s*exit\b'
    }
    It 'contains only ASCII, so Windows PowerShell 5.1 reads it without a BOM' {
        $script:ScriptText | Should -Not -Match '[^\x00-\x7F]'
    }
    It 'pins the same Authenticode signer CI verifies' {
        $verify = Get-Content -LiteralPath (Join-Path $script:ScriptsDir 'verify-windows-signing.sh') -Raw
        $expected = [regex]::Match($verify, "(?m)^EXPECTED_CN_PROD='(.+)'$").Groups[1].Value
        $expected | Should -Not -BeNullOrEmpty
        $pinned = [regex]::Match($script:ScriptText, '(?m)^\$ExpectedSigner = "(.+)"$').Groups[1].Value
        $pinned | Should -BeExactly $expected
    }
    It 'pins the same release key fingerprint install.sh verifies' {
        $sh = Get-Content -LiteralPath (Join-Path $script:ScriptsDir 'install.sh') -Raw
        $expected = [regex]::Match($sh, '(?m)^RELEASE_KEY_FINGERPRINT="([0-9A-F]{40})"$').Groups[1].Value
        $expected | Should -Not -BeNullOrEmpty
        $pinned = [regex]::Match($script:ScriptText, '(?m)^\$ReleaseKeyFingerprint = "([0-9A-F]{40})"$').Groups[1].Value
        $pinned | Should -BeExactly $expected
    }
}

Describe 'Get-ExpectedSha256' {
    It 'returns the digest for a text-mode entry' {
        $sums = Write-SumsFile @("$script:ExeHash  $script:Artifact")
        Get-ExpectedSha256 -SumsPath $sums -Name $script:Artifact | Should -BeExactly $script:ExeHash
    }
    It 'returns the digest for a binary-mode entry (leading asterisk)' {
        $sums = Write-SumsFile @("$script:ExeHash *$script:Artifact")
        Get-ExpectedSha256 -SumsPath $sums -Name $script:Artifact | Should -BeExactly $script:ExeHash
    }
    It 'lowercases an uppercase digest' {
        $sums = Write-SumsFile @("$($script:ExeHash.ToUpperInvariant())  $script:Artifact")
        Get-ExpectedSha256 -SumsPath $sums -Name $script:Artifact | Should -BeExactly $script:ExeHash
    }
    It 'picks the right line among several' {
        $other = 'a' * 64
        $sums = Write-SumsFile @(
            "$other  quip-node-manager-linux-x86_64.AppImage",
            "$script:ExeHash  $script:Artifact",
            "$other  quip-node-manager-macos-universal.dmg")
        Get-ExpectedSha256 -SumsPath $sums -Name $script:Artifact | Should -BeExactly $script:ExeHash
    }
    It 'returns null when the file has no entry for the name' {
        $sums = Write-SumsFile @("$script:ExeHash  quip-node-manager-linux-x86_64.AppImage")
        Get-ExpectedSha256 -SumsPath $sums -Name $script:Artifact | Should -BeNullOrEmpty
    }
    It 'does not match a name that only shares a prefix' {
        $sums = Write-SumsFile @("$script:ExeHash  $script:Artifact.sig")
        Get-ExpectedSha256 -SumsPath $sums -Name $script:Artifact | Should -BeNullOrEmpty
    }
    It 'accepts CRLF line endings' {
        $sums = Join-Path $script:Work 'SHA256SUMS'
        [System.IO.File]::WriteAllText($sums, "$script:ExeHash  $script:Artifact`r`n")
        Get-ExpectedSha256 -SumsPath $sums -Name $script:Artifact | Should -BeExactly $script:ExeHash
    }
    It 'ignores a line that is not a sha256sum record' {
        $sums = Write-SumsFile @("<html>redirect page</html>", "$script:ExeHash  $script:Artifact")
        Get-ExpectedSha256 -SumsPath $sums -Name $script:Artifact | Should -BeExactly $script:ExeHash
    }
}

Describe 'Confirm-FileSha256' {
    It 'passes when the digest matches' {
        $sums = Write-SumsFile @("$script:ExeHash  $script:Artifact")
        { Confirm-FileSha256 -Path $script:Exe -SumsPath $sums -Name $script:Artifact } | Should -Not -Throw
    }
    It 'passes when SHA256SUMS lists the digest in uppercase' {
        $sums = Write-SumsFile @("$($script:ExeHash.ToUpperInvariant())  $script:Artifact")
        { Confirm-FileSha256 -Path $script:Exe -SumsPath $sums -Name $script:Artifact } | Should -Not -Throw
    }
    It 'throws on a mismatch' {
        $sums = Write-SumsFile @("$('0' * 64)  $script:Artifact")
        { Confirm-FileSha256 -Path $script:Exe -SumsPath $sums -Name $script:Artifact } |
            Should -Throw -ExpectedMessage '*Checksum mismatch*'
    }
    It 'throws when the file has no entry' {
        $sums = Write-SumsFile @("$script:ExeHash  something-else.exe")
        { Confirm-FileSha256 -Path $script:Exe -SumsPath $sums -Name $script:Artifact } |
            Should -Throw -ExpectedMessage '*no entry*'
    }
}

Describe 'Test-SubjectHasCN' {
    It 'is true when CN is the first RDN' {
        Test-SubjectHasCN 'CN=Acme, O=Acme, C=US' | Should -BeTrue
    }
    It 'is true when CN is the last RDN' {
        Test-SubjectHasCN 'C=US, O=Acme, CN=Acme' | Should -BeTrue
    }
    It 'is false when the subject has no CN' {
        Test-SubjectHasCN 'O=Acme, C=US' | Should -BeFalse
    }
    It 'is false when CN= appears only inside a quoted value' {
        Test-SubjectHasCN 'O=Acme, L="Town, CN=Decoy", C=US' | Should -BeFalse
    }
    It 'is false when CN= appears on a line inside a quoted value' {
        Test-SubjectHasCN "O=`"Acme`nCN=Decoy`nSupport`", C=US" | Should -BeFalse
    }
    It 'is true when a quoted value with doubled quotes precedes the CN' {
        Test-SubjectHasCN 'O="He said ""CN=x""", CN=Acme' | Should -BeTrue
    }
}

Describe 'Confirm-AuthenticodeSigner' {
    BeforeAll {
        # Get-AuthenticodeSignature exists only on Windows. The stub returns
        # whatever $script:FakeSignature holds. The fake certificate carries
        # the Subject string the message quotes and answers GetNameInfo with
        # the CN that .NET would decode from the real certificate.
        function script:Get-AuthenticodeSignature { return $script:FakeSignature }
        function script:New-FakeSignature([string]$Status, [string]$Subject, [string]$CN) {
            $cert = [pscustomobject]@{ Subject = $Subject } |
                Add-Member -MemberType ScriptMethod -Name GetNameInfo -Value { $CN }.GetNewClosure() -PassThru
            [pscustomobject]@{
                Status            = $Status
                SignerCertificate = $cert
            }
        }
        $script:CN = 'HADAMARD GATE INCORPORATED'
    }
    It 'passes a valid signature whose leaf CN matches' {
        $script:FakeSignature = New-FakeSignature 'Valid' "CN=$script:CN, O=$script:CN, L=Baltimore, S=Maryland, C=US" $script:CN
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } | Should -Not -Throw
    }
    It 'passes when CN is the last RDN' {
        $script:FakeSignature = New-FakeSignature 'Valid' "C=US, CN=$script:CN" $script:CN
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } | Should -Not -Throw
    }
    It 'passes when the CN value is quoted' {
        $script:FakeSignature = New-FakeSignature 'Valid' "CN=`"$script:CN`", C=US" $script:CN
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } | Should -Not -Throw
    }
    It 'throws when the file is unsigned' {
        $script:FakeSignature = New-FakeSignature 'NotSigned' '' ''
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } |
            Should -Throw -ExpectedMessage '*NotSigned*'
    }
    It 'throws when the chain does not verify (sandbox signature)' {
        $script:FakeSignature = New-FakeSignature 'UnknownError' "CN=Esigner LLC, C=US" 'Esigner LLC'
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } |
            Should -Throw -ExpectedMessage '*UnknownError*'
    }
    It 'throws when a valid signature names another publisher' {
        $script:FakeSignature = New-FakeSignature 'Valid' "CN=Somebody Else, O=Somebody Else, C=US" 'Somebody Else'
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } |
            Should -Throw -ExpectedMessage "*Somebody Else*not by $script:CN*"
    }
    It 'rejects a CN that merely starts with the expected name' {
        $script:FakeSignature = New-FakeSignature 'Valid' "CN=$script:CN LLC, C=US" "$script:CN LLC"
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } |
            Should -Throw -ExpectedMessage "*not by $script:CN*"
    }
    It 'rejects the expected name when it appears only in O, not CN' {
        $script:FakeSignature = New-FakeSignature 'Valid' "CN=Reseller, O=$script:CN, C=US" 'Reseller'
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } |
            Should -Throw -ExpectedMessage "*not by $script:CN*"
    }
    It 'rejects the expected name hidden inside a quoted O value' {
        $script:FakeSignature = New-FakeSignature 'Valid' "CN=Reseller, O=`"Acme, CN=$script:CN, Support`", C=US" 'Reseller'
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } |
            Should -Throw -ExpectedMessage "*not by $script:CN*"
    }
    It 'rejects a CN whose literal value carries quotes' {
        $script:FakeSignature = New-FakeSignature 'Valid' "CN=`"`"`"$script:CN`"`"`", C=US" "`"$script:CN`""
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } |
            Should -Throw -ExpectedMessage "*not by $script:CN*"
    }
    It 'rejects the expected name on a line inside a quoted O value' {
        $script:FakeSignature = New-FakeSignature 'Valid' "CN=Reseller, O=`"Acme`nCN=$script:CN`nSupport`", C=US" 'Reseller'
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } |
            Should -Throw -ExpectedMessage "*not by $script:CN*"
    }
    It 'rejects a leaf with no CN even when SimpleName falls back to a matching O' {
        $script:FakeSignature = New-FakeSignature 'Valid' "O=$script:CN, C=US" $script:CN
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } |
            Should -Throw -ExpectedMessage "*not by $script:CN*"
    }
    It 'rejects a leaf with no CN when a quoted attribute contains CN=' {
        $script:FakeSignature = New-FakeSignature 'Valid' "O=$script:CN, L=`"Acme, CN=Decoy`", C=US" $script:CN
        { Confirm-AuthenticodeSigner -Path $script:Exe -ExpectedCN $script:CN } |
            Should -Throw -ExpectedMessage "*not by $script:CN*"
    }
}

Describe 'Confirm-SumsSignature' {
    BeforeAll {
        $script:FPR = 'A63860E21E7070C2C26FDA5DC85BEAB01AD9FEE3'
        # Stubs stand in for the gpg and gpgconf executables. A function wins
        # over an executable of the same name, so `& gpg` reaches the stub.
        # $script:GpgExit maps a sub-command (import, list-keys, verify) to the
        # exit code the stub reports; $script:GpgFprLine is what --list-keys
        # prints.
        function script:gpg {
            $cmd = ($args | Where-Object { $_ -in '--import', '--list-keys', '--verify' } | Select-Object -First 1)
            $script:GpgCalls += , @($args)
            $global:LASTEXITCODE = if ($script:GpgExit.ContainsKey($cmd)) { $script:GpgExit[$cmd] } else { 0 }
            if ($cmd -eq '--list-keys' -and $global:LASTEXITCODE -eq 0) {
                'tru::1:1700000000:0:3:1:5'
                'pub:u:255:22:C85BEAB01AD9FEE3:1700000000:::u:::scESC:::::ed25519:::0:'
                $script:GpgFprLine
            }
        }
        function script:gpgconf { $script:GpgconfCalls += ($args -join ' '); $global:LASTEXITCODE = 0 }
        $script:Sums = Join-Path $script:Work 'SHA256SUMS'
        Set-Content -LiteralPath $script:Sums -Value "$script:ExeHash  $script:Artifact"
        $script:Sig = Join-Path $script:Work 'SHA256SUMS.asc'
        Set-Content -LiteralPath $script:Sig -Value '-----BEGIN PGP SIGNATURE-----'
        $script:Key = Join-Path $script:Work 'key.asc'
        Set-Content -LiteralPath $script:Key -Value '-----BEGIN PGP PUBLIC KEY BLOCK-----'
    }
    BeforeEach {
        $script:GpgCalls = @()
        $script:GpgconfCalls = @()
        $script:GpgExit = @{}
        $script:GpgFprLine = "fpr:::::::::$($script:FPR):"
    }
    It 'passes when the key fingerprint matches and the signature verifies' {
        { Confirm-SumsSignature -SumsPath $script:Sums -SigPath $script:Sig -KeyPath $script:Key -Fingerprint $script:FPR } |
            Should -Not -Throw
        @($script:GpgCalls | Where-Object { $_ -contains '--verify' }).Count | Should -Be 1
    }
    It 'reads the primary key fingerprint, not a subkey, when both are listed' {
        $script:GpgFprLine = @(
            "fpr:::::::::$($script:FPR):",
            'sub:u:255:18:0123456789ABCDEF:1700000000::::::e::::::cv25519::',
            'fpr:::::::::DEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF:')
        { Confirm-SumsSignature -SumsPath $script:Sums -SigPath $script:Sig -KeyPath $script:Key -Fingerprint $script:FPR } |
            Should -Not -Throw
    }
    It 'rejects a key file that carries a second primary key' {
        $script:GpgFprLine = @(
            "fpr:::::::::$($script:FPR):",
            'pub:u:255:22:DEADBEEFDEADBEEF:1700000000:::u:::scESC:::::ed25519:::0:',
            'fpr:::::::::DEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF:')
        { Confirm-SumsSignature -SumsPath $script:Sums -SigPath $script:Sig -KeyPath $script:Key -Fingerprint $script:FPR } |
            Should -Throw -ExpectedMessage '*2 primary keys, expected 1*'
        @($script:GpgCalls | Where-Object { $_ -contains '--verify' }).Count | Should -Be 0
    }
    It 'throws when the key cannot be imported' {
        $script:GpgExit['--import'] = 2
        { Confirm-SumsSignature -SumsPath $script:Sums -SigPath $script:Sig -KeyPath $script:Key -Fingerprint $script:FPR } |
            Should -Throw -ExpectedMessage '*Could not read the release signing key*'
    }
    It 'throws when the key cannot be listed back' {
        $script:GpgExit['--list-keys'] = 2
        { Confirm-SumsSignature -SumsPath $script:Sums -SigPath $script:Sig -KeyPath $script:Key -Fingerprint $script:FPR } |
            Should -Throw -ExpectedMessage '*read the release key back*'
    }
    It 'throws when the fingerprint differs, naming both' {
        $script:GpgFprLine = 'fpr:::::::::DEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF:'
        { Confirm-SumsSignature -SumsPath $script:Sums -SigPath $script:Sig -KeyPath $script:Key -Fingerprint $script:FPR } |
            Should -Throw -ExpectedMessage "*DEADBEEF*expected $script:FPR*"
        @($script:GpgCalls | Where-Object { $_ -contains '--verify' }).Count | Should -Be 0
    }
    It 'throws when gpg reports no fingerprint' {
        $script:GpgFprLine = 'uid:u::::1700000000::ABCDEF::Quip Release::::::::::0:'
        { Confirm-SumsSignature -SumsPath $script:Sums -SigPath $script:Sig -KeyPath $script:Key -Fingerprint $script:FPR } |
            Should -Throw -ExpectedMessage '*no fingerprint*'
    }
    It 'throws when the signature does not verify' {
        $script:GpgExit['--verify'] = 1
        { Confirm-SumsSignature -SumsPath $script:Sums -SigPath $script:Sig -KeyPath $script:Key -Fingerprint $script:FPR } |
            Should -Throw -ExpectedMessage '*not valid*'
    }
    It 'stops the gpg daemons and removes the temporary home on failure' {
        $script:GpgExit['--verify'] = 1
        { Confirm-SumsSignature -SumsPath $script:Sums -SigPath $script:Sig -KeyPath $script:Key -Fingerprint $script:FPR } | Should -Throw -ExpectedMessage '*not valid*'
        $script:GpgconfCalls.Count | Should -Be 1
        $first = $script:GpgCalls[0]
        $gpgHome = $first[[array]::IndexOf($first, '--homedir') + 1]
        $gpgHome | Should -Match 'quip-gnupg-'
        Test-Path -LiteralPath $gpgHome | Should -BeFalse
    }
    It 'removes the temporary home when the temp path contains brackets' {
        $bracketTemp = Join-Path $script:Work 'tmp[1]'
        New-Item -ItemType Directory -Path $bracketTemp | Out-Null
        $savedTmpdir = $env:TMPDIR
        $savedTemp = $env:TEMP
        try {
            $env:TMPDIR = $bracketTemp
            $env:TEMP = $bracketTemp
            $script:GpgExit['--verify'] = 1
            { Confirm-SumsSignature -SumsPath $script:Sums -SigPath $script:Sig -KeyPath $script:Key -Fingerprint $script:FPR } |
                Should -Throw -ExpectedMessage '*not valid*'
        } finally {
            $env:TMPDIR = $savedTmpdir
            $env:TEMP = $savedTemp
        }
        $first = $script:GpgCalls[0]
        $gpgHome = $first[[array]::IndexOf($first, '--homedir') + 1]
        $gpgHome | Should -BeLike '*tmp`[1`]*'
        Test-Path -LiteralPath $gpgHome | Should -BeFalse
    }
}
