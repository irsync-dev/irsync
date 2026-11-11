#!/bin/bash
# Installs what the packaging scripts and their tests need on an Ubuntu
# runner: apt-ftparchive, createrepo_c, rpm, gpg, and nfpm at a pinned
# version, checked against its SHA-256. Puts nfpm on the PATH of later steps.
set -euo pipefail

NFPM_VERSION=2.48.0
NFPM_SHA256=6d7f9a28b2f397934403950fcbba83f598b08d02edbe79a0815b3b4897341750

sudo apt-get update
sudo apt-get install -y --no-install-recommends apt-utils createrepo-c rpm gnupg

url="https://github.com/goreleaser/nfpm/releases/download/v${NFPM_VERSION}/nfpm_${NFPM_VERSION}_Linux_x86_64.tar.gz"
curl -fsSL -o "$RUNNER_TEMP/nfpm.tar.gz" "$url"
echo "$NFPM_SHA256  $RUNNER_TEMP/nfpm.tar.gz" | sha256sum --check -
mkdir -p "$RUNNER_TEMP/nfpm"
tar -xzf "$RUNNER_TEMP/nfpm.tar.gz" -C "$RUNNER_TEMP/nfpm" nfpm
echo "$RUNNER_TEMP/nfpm" >> "$GITHUB_PATH"
