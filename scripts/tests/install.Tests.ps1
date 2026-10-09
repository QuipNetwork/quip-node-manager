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
