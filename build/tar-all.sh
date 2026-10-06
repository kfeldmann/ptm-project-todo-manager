#!/bin/sh

DS="$(date +%Y%m%d%H%M)"
PROJ_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$PROJ_DIR"

tar -czf "ptm-binaries-${DS}.tgz" \
  target/{aarch64-apple-darwin,x86_64-unknown-linux-musl}/release/{ptm,ptm-sync,md}

ls -l "ptm-binaries-${DS}.tgz"
