# SPDX-License-Identifier: AGPL-3.0-or-later

.PHONY: fetch-submodules submodules builder-image

fetch-submodules:
	git submodule update --init --recursive

submodules: fetch-submodules

# CI toolchain image for build-linux-x86_64. The `build-toolchain-image` CI job
# normally builds and pushes this; use this target only as a fallback when the
# runners are unavailable. Needs `docker login registry.gitlab.com` first.
# After pushing, bump the pinned digest in TOOLCHAIN_IMAGE in .gitlab-ci.yml —
# CI resolves the digest, not the tag.
BUILDER_IMAGE ?= registry.gitlab.com/quip.network/quip-node-manager/ci-toolchain:latest

builder-image:
	docker buildx build \
		--platform linux/amd64 \
		--file .gitlab/ci-toolchain.Dockerfile \
		--tag $(BUILDER_IMAGE) \
		--push \
		.gitlab/

# -- Script lint and tests ----------------------------------------------------
# The same checks the `shellcheck` and `powershell-lint` CI jobs run. The
# PowerShell targets run in the CI image through Docker, so no local pwsh is
# needed. Each run installs the two modules from the PowerShell Gallery at
# the pinned versions.
POWERSHELL_IMAGE ?= mcr.microsoft.com/powershell:7.5-debian-12
PSSCRIPTANALYZER_VERSION ?= 1.25.0
PESTER_VERSION ?= 6.2.0

.PHONY: lint-shell lint-powershell test-powershell

lint-shell:
	shellcheck scripts/*.sh

lint-powershell:
	docker run --rm -v "$(CURDIR)":/work -w /work $(POWERSHELL_IMAGE) pwsh -NoLogo -NoProfile -Command \
	  "Install-Module -Name PSScriptAnalyzer -RequiredVersion $(PSSCRIPTANALYZER_VERSION) -Force -Scope CurrentUser; \
	   Invoke-ScriptAnalyzer -Path scripts -Recurse -Settings scripts/PSScriptAnalyzerSettings.psd1 -EnableExit"
