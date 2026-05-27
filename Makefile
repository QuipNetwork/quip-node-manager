# SPDX-License-Identifier: AGPL-3.0-or-later

.PHONY: fetch-submodules submodules

fetch-submodules:
	git submodule update --init --recursive

submodules: fetch-submodules
