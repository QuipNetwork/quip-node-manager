# syntax=docker/dockerfile:1.7
#
# CI toolchain image: ubuntu:22.04 plus everything `bun run tauri build` needs
# on Linux — the GTK/WebKit system libraries, a pinned Rust toolchain, and bun.
#
# The Linux build jobs in `.gitlab-ci.yml` pull this image from this project's
# own registry. Before it existed the x86_64 job ran on a stock ubuntu:22.04 and
# re-paid the whole setup on every run: an apt-get of ~120 packages, a rustup
# install, and a bun install. `GIT_CLEAN_FLAGS` already preserves
# src-tauri/target and .cargo-cache, so the compile was warm while the setup
# was not — three concurrent pipelines spent over 40 minutes each in apt
# without reaching a single crate.
#
# The `build-toolchain-image` jobs build and push this with kaniko. They run
# automatically when this file changes and are manual otherwise. Unlike the
# validator's toolchain image, which lives on Docker Hub and must be pushed
# from a workstation because CI holds no Docker Hub credentials, every job
# here is already authenticated to $CI_REGISTRY through the job token.
#
# After a job publishes, bump its pinned digest in `.gitlab-ci.yml` — CI
# resolves the digest, not the tag, so a re-push alone changes nothing. Each
# job prints the digest it pushed and the variable that holds it.
#
# `make builder-image` builds and pushes the same image from a workstation.
# That path is a fallback for when the runners are unavailable, and it needs a
# `docker login registry.gitlab.com` first.
#
# One Dockerfile, two images. `build-toolchain-image` builds it on an amd64
# runner and `build-toolchain-image-arm64` builds it on an arm64 runner. Each
# image is single-arch and has its own pinned digest (`TOOLCHAIN_IMAGE` and
# `TOOLCHAIN_IMAGE_ARM64`), because a Tauri bundle must be built on the
# architecture it targets. The rustup and bun installers below select the
# architecture of the machine they run on.

# Ubuntu 22.04, not the Debian bookworm base the validator's toolchain uses.
# The AppImage and .deb inherit their glibc floor from whatever builds them,
# and bookworm's 2.36 would drop every user still on Ubuntu 22.04 (2.35).
# Bump this only alongside a deliberate decision to raise that floor.
FROM ubuntu:22.04

# Pin the toolchain so a job's Rust version changes when this image is rebuilt
# rather than silently mid-week. The `rustfmt` lint job still tracks floating
# stable on `rust:slim`, so a new stable can fail formatting on unchanged code
# before this pin moves; the fix there is to run the formatter, as before.
ARG RUST_VERSION=1.98.0
# Nothing watches this file for updates. Check https://bun.sh/ on rebuild.
ARG BUN_VERSION=1.3.11

ENV DEBIAN_FRONTEND=noninteractive

# The GTK/WebKit set is what Tauri v2 links against on Linux; patchelf and
# librsvg2-dev are what the AppImage and .deb bundlers shell out to. Pinning
# system library versions across Ubuntu point releases is brittle and this
# image is build tooling only.
# hadolint ignore=DL3008
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl file git unzip \
        libayatana-appindicator3-dev libgtk-3-dev librsvg2-dev \
        libssl-dev libwebkit2gtk-4.1-dev patchelf pkg-config xdg-utils \
    && rm -rf /var/lib/apt/lists/*

# Install under /usr/local so the job's `CARGO_HOME` override redirects only
# the package cache into the preserved project directory. The rustup shims
# resolve the toolchain through RUSTUP_HOME, which stays here.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    BUN_INSTALL=/usr/local/bun \
    PATH=/usr/local/cargo/bin:/usr/local/bun/bin:$PATH

# Both installers below pipe curl into a shell. Under the default `sh -c` a
# failed download still exits 0, because the pipeline reports only the shell's
# status — the image would ship without the toolchain it just "installed".
SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --no-modify-path --profile minimal \
        --default-toolchain "${RUST_VERSION}" \
    && chmod -R a+w "${RUSTUP_HOME}" "${CARGO_HOME}"

RUN curl -fsSL https://bun.sh/install | bash -s -- "bun-v${BUN_VERSION}" \
    && chmod -R a+w "${BUN_INSTALL}"

# Fail the image build here rather than in a CI job three minutes into a pull.
RUN cargo --version && rustc --version && bun --version
