# SPDX-License-Identifier: AGPL-3.0-or-later
# Runs the Pester tests in this directory and exits non-zero on any failure.
# Needs Pester 6.2.0 or later:
#   Install-Module -Name Pester -RequiredVersion 6.2.0 -Force -Scope CurrentUser
# MinimumVersion keeps Windows' bundled Pester 3.4.0 from being picked up.
Import-Module Pester -MinimumVersion 6.2.0
$config = New-PesterConfiguration
$config.Run.Path = $PSScriptRoot
$config.Run.Exit = $true
$config.Output.Verbosity = 'Detailed'
Invoke-Pester -Configuration $config
