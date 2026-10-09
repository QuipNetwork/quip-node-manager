# SPDX-License-Identifier: AGPL-3.0-or-later
# PSScriptAnalyzer settings for scripts/*.ps1 and scripts/tests/*.ps1.
@{
    # install.ps1 is an interactive installer: its only output is progress for
    # the person running it. Write-Host is the right cmdlet for that, and
    # Write-Information is hidden by default under `irm | iex`.
    ExcludeRules = @('PSAvoidUsingWriteHost')
}
