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
}
