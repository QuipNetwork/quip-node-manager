# SPDX-License-Identifier: AGPL-3.0-or-later

.PHONY: fetch-submodules submodules builder-image

fetch-submodules:
	git submodule update --init --recursive

submodules: fetch-submodules

# CI toolchain image for build-linux-x86_64. Built and pushed by hand from a
# workstation, because no Docker Hub credentials exist in the project or group
# CI variables. After pushing, bump the pinned digest in TOOLCHAIN_IMAGE in
# .gitlab-ci.yml — CI resolves the digest, not the tag.
BUILDER_IMAGE ?= carback1/quip-tauri-builder:latest

builder-image:
	docker buildx build \
		--platform linux/amd64 \
		--file .gitlab/ci-toolchain.Dockerfile \
		--tag $(BUILDER_IMAGE) \
		--push \
		.gitlab/
