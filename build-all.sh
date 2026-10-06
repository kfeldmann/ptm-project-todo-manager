#!/bin/sh

set -e

./build-linux-musl-x86_64 ptm
./build-linux-musl-x86_64 ptm-sync
./build-linux-musl-x86_64 md
./build-macos-arm ptm
./build-macos-arm ptm-sync
./build-macos-arm md

ls -l target/{aarch64-apple-darwin,x86_64-unknown-linux-musl}/release/{ptm,ptm-sync,md}
