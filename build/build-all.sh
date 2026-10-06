#!/bin/sh

set -e

BUILD_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJ_DIR="$(cd "${BUILD_DIR}/.." && pwd)"

"${BUILD_DIR}/build-linux-musl-x86_64" ptm
"${BUILD_DIR}/build-linux-musl-x86_64" ptm-sync
"${BUILD_DIR}/build-linux-musl-x86_64" md
"${BUILD_DIR}/build-macos-arm" ptm
"${BUILD_DIR}/build-macos-arm" ptm-sync
"${BUILD_DIR}/build-macos-arm" md

ls -l "${PROJ_DIR}"/target/{aarch64-apple-darwin,x86_64-unknown-linux-musl}/release/{ptm,ptm-sync,md}
