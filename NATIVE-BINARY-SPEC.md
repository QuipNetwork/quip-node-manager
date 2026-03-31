<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Native Binary Distribution Spec for quip-protocol

## Overview

quip-node-manager supports a native execution mode (non-Docker) that requires
pre-built, frozen binaries of `quip-network-node`. This document specifies how
to produce those binaries using PyInstaller, how to integrate them into CI, and
how quip-node-manager downloads and launches them at runtime.

## Entry Point

```
quip_cli:network_node_main
```

The PyInstaller invocation targets this callable as the entry point for the
frozen binary.

## Hidden Imports

PyInstaller's static analysis misses dynamically loaded submodules. The
following must be declared as hidden imports:

- `shared.*` -- common utilities, config parsing, networking
- `CPU.*` -- CPU-based solver backends
- `GPU.*` -- Metal and CUDA solver backends
- `QPU.*` -- D-Wave QPU solver backends

## Data Files

These non-Python assets must be bundled into the frozen binary:

| Source Pattern | Description |
|----------------|-------------|
| `GPU/*.cu` | CUDA kernel source files |
| `GPU/*.metal` | Metal shader source files |
| `dwave_topologies/topologies/*.json.gz` | D-Wave hardware topology graphs |
| `dwave_topologies/embeddings/*.json.gz` | Pre-computed minor embeddings |
| `genesis_block_public.json` | Genesis block for chain validation |

## Platform Build Matrix

| Platform | Arch | Extras | Notes |
|----------|------|--------|-------|
| macOS | arm64 | `[metal]` | pyobjc frameworks, Metal shaders |
| macOS | x86_64 | base | No Metal on Intel Macs |
| Linux | x86_64 | `[cuda]` | cupy-cuda12x, nvidia-ml-py |
| Windows | x86_64 | `[cuda]` | cupy-cuda12x, nvidia-ml-py |

## Output Naming

Binaries follow the pattern:

```
quip-network-node-{os}-{arch}[.exe]
```

Examples:

- `quip-network-node-macos-arm64`
- `quip-network-node-macos-x86_64`
- `quip-network-node-linux-x86_64`
- `quip-network-node-windows-x86_64.exe`

## Version Embedding

The version string is read from `pyproject.toml` at build time and embedded
into the binary. It is queryable at runtime:

```bash
./quip-network-node --version
# quip-network-node 1.2.3
```

## Size Targets

| Variant | Expected Size |
|---------|---------------|
| Without CUDA (macOS, Linux base) | ~100--200 MB |
| With CUDA (Linux, Windows) | ~400--600 MB |

## Sample PyInstaller .spec File

```python
# quip-network-node.spec
# -*- mode: python ; coding: utf-8 -*-
import os
import tomllib
from pathlib import Path

# Read version from pyproject.toml
with open("pyproject.toml", "rb") as f:
    version = tomllib.load(f)["project"]["version"]

block_cipher = None

a = Analysis(
    ["quip_cli/network_node_main.py"],
    pathex=[],
    binaries=[],
    datas=[
        ("GPU/*.cu", "GPU"),
        ("GPU/*.metal", "GPU"),
        ("dwave_topologies/topologies/*.json.gz", "dwave_topologies/topologies"),
        ("dwave_topologies/embeddings/*.json.gz", "dwave_topologies/embeddings"),
        ("genesis_block_public.json", "."),
    ],
    hiddenimports=[
        "shared",
        "shared.*",
        "CPU",
        "CPU.*",
        "GPU",
        "GPU.*",
        "QPU",
        "QPU.*",
    ],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    win_no_prefer_redirects=False,
    win_private_assemblies=False,
    cipher=block_cipher,
    noarchive=False,
)

pyz = PYZ(a.pure, a.zipped_data, cipher=block_cipher)

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.zipfiles,
    a.datas,
    [],
    name=f"quip-network-node",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=True,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
)
```

## Sample GitLab CI Pipeline

```yaml
stages:
  - build

.pyinstaller-base:
  stage: build
  script:
    - pip install pyinstaller
    - pip install ".[${EXTRAS}]"
    - pyinstaller quip-network-node.spec
    - ./dist/quip-network-node${EXE_EXT} --version
  artifacts:
    paths:
      - dist/quip-network-node*
    expire_in: 30 days

build-macos-arm64:
  extends: .pyinstaller-base
  tags: [macos, arm64]
  variables:
    EXTRAS: "metal"
    EXE_EXT: ""
  after_script:
    - mv dist/quip-network-node dist/quip-network-node-macos-arm64

build-macos-x86_64:
  extends: .pyinstaller-base
  tags: [macos, x86_64]
  variables:
    EXTRAS: ""
    EXE_EXT: ""
  after_script:
    - mv dist/quip-network-node dist/quip-network-node-macos-x86_64

build-linux-x86_64:
  extends: .pyinstaller-base
  tags: [linux, x86_64]
  image: python:3.12-bookworm
  variables:
    EXTRAS: "cuda"
    EXE_EXT: ""
  after_script:
    - mv dist/quip-network-node dist/quip-network-node-linux-x86_64

build-windows-x86_64:
  extends: .pyinstaller-base
  tags: [windows, x86_64]
  variables:
    EXTRAS: "cuda"
    EXE_EXT: ".exe"
  after_script:
    - mv dist/quip-network-node.exe dist/quip-network-node-windows-x86_64.exe

release-binaries:
  stage: build
  needs:
    - build-macos-arm64
    - build-macos-x86_64
    - build-linux-x86_64
    - build-windows-x86_64
  rules:
    - if: $CI_COMMIT_TAG
  script:
    - echo "Uploading binaries to release $CI_COMMIT_TAG"
  release:
    tag_name: $CI_COMMIT_TAG
    description: "Release $CI_COMMIT_TAG"
    assets:
      links:
        - name: quip-network-node-macos-arm64
          url: "${CI_PROJECT_URL}/-/jobs/${CI_JOB_ID}/artifacts/file/dist/quip-network-node-macos-arm64"
          filepath: /quip-network-node-macos-arm64
        - name: quip-network-node-macos-x86_64
          url: "${CI_PROJECT_URL}/-/jobs/${CI_JOB_ID}/artifacts/file/dist/quip-network-node-macos-x86_64"
          filepath: /quip-network-node-macos-x86_64
        - name: quip-network-node-linux-x86_64
          url: "${CI_PROJECT_URL}/-/jobs/${CI_JOB_ID}/artifacts/file/dist/quip-network-node-linux-x86_64"
          filepath: /quip-network-node-linux-x86_64
        - name: quip-network-node-windows-x86_64.exe
          url: "${CI_PROJECT_URL}/-/jobs/${CI_JOB_ID}/artifacts/file/dist/quip-network-node-windows-x86_64.exe"
          filepath: /quip-network-node-windows-x86_64.exe
```

## Smoke Test

Every CI build must pass this check before the artifact is uploaded:

```bash
./quip-network-node --version
# Must exit with code 0 and print the embedded version string.
```

## Release Distribution

Binaries are attached to GitLab release tags. quip-node-manager downloads
them using the permalink pattern:

```
https://gitlab.com/piqued/quip-protocol/-/releases/permalink/latest/downloads/quip-network-node-{os}-{arch}
```

Examples:

```
https://gitlab.com/piqued/quip-protocol/-/releases/permalink/latest/downloads/quip-network-node-macos-arm64
https://gitlab.com/piqued/quip-protocol/-/releases/permalink/latest/downloads/quip-network-node-linux-arm64
https://gitlab.com/piqued/quip-protocol/-/releases/permalink/latest/downloads/quip-network-node-windows-arm64.exe
https://gitlab.com/piqued/quip-protocol/-/releases/permalink/latest/downloads/quip-network-node-linux-x86_64
https://gitlab.com/piqued/quip-protocol/-/releases/permalink/latest/downloads/quip-network-node-windows-x86_64.exe
```
