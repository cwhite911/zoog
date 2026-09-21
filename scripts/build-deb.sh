#!/usr/bin/env bash
# Builds the Zoog .deb (PLAN.md Phase 7). Requires cargo-deb.
set -eu
cd "$(dirname "$0")/.."
cargo build --release -p lab-gui -p labctl
cargo deb -p lab-gui --no-build
ls -lh target/debian/*.deb
