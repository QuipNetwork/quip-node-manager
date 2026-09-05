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
