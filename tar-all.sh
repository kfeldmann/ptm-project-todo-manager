#!/bin/sh

DS="$(date +%Y%m%d%H%M)"

tar -czf "ptm-binaries-${DS}.tgz" \
  target/{aarch64-apple-darwin,x86_64-unknown-linux-musl}/release/{ptm,ptm-sync,md}

ls -l "ptm-binaries-${DS}.tgz"
